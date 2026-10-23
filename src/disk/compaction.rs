use crate::disk::manifest::{FileMetaData, Version, VersionEdit};
use crate::disk::merge::{MergeIterator, StorageIterator};
use crate::disk::sstable::SSTableWriter;
use std::fs::File;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub struct CompactionJob {
    pub input_level: usize,
    pub output_level: usize,
    pub inputs_level: Vec<FileMetaData>,
    pub inputs_next_level: Vec<FileMetaData>,
}

#[derive(Clone, Debug)]
pub struct CompactionPicker {
    pub l0_trigger: usize,
    pub base_level_size: u64,
    pub level_size_multiplier: u64,
}

impl Default for CompactionPicker {
    fn default() -> Self {
        Self {
            l0_trigger: 4,
            base_level_size: 10 * 1024 * 1024,
            level_size_multiplier: 10,
        }
    }
}

const fn user_key(internal_key: &[u8]) -> &[u8] {
    if internal_key.len() >= 9 {
        let (k, _) = internal_key.split_at(internal_key.len() - 9);
        k
    } else {
        internal_key
    }
}

fn files_overlap(a: &FileMetaData, min_u: &[u8], max_u: &[u8]) -> bool {
    let a_min = user_key(&a.min_key);
    let a_max = user_key(&a.max_key);
    a_min <= max_u && min_u <= a_max
}

impl CompactionPicker {
    #[must_use]
    pub fn pick_compaction(&self, version: &Version) -> Option<CompactionJob> {
        let num_levels = version.files.len();
        if num_levels < 2 {
            return None;
        }

        // 1. Check Level 0 trigger
        if version.files[0].len() >= self.l0_trigger {
            let inputs_level = version.files[0].clone();
            let mut min_u = None;
            let mut max_u = None;

            for f in &inputs_level {
                let f_min = user_key(&f.min_key);
                let f_max = user_key(&f.max_key);
                if min_u.is_none() || f_min < min_u.unwrap() {
                    min_u = Some(f_min);
                }
                if max_u.is_none() || f_max > max_u.unwrap() {
                    max_u = Some(f_max);
                }
            }

            let mut inputs_next_level = Vec::new();
            if let (Some(min_u), Some(max_u)) = (min_u, max_u) {
                for f in &version.files[1] {
                    if files_overlap(f, min_u, max_u) {
                        inputs_next_level.push(f.clone());
                    }
                }
            }

            return Some(CompactionJob {
                input_level: 0,
                output_level: 1,
                inputs_level,
                inputs_next_level,
            });
        }

        // 2. Check Level i (i > 0) size triggers
        let mut level_max_bytes = self.base_level_size;
        for i in 1..num_levels - 1 {
            let total_size: u64 = version.files[i].iter().map(|f| f.file_size).sum();
            if total_size > level_max_bytes && !version.files[i].is_empty() {
                let file_to_compact = version.files[i][0].clone();
                let f_min = user_key(&file_to_compact.min_key);
                let f_max = user_key(&file_to_compact.max_key);

                let mut inputs_next_level = Vec::new();
                for f in &version.files[i + 1] {
                    if files_overlap(f, f_min, f_max) {
                        inputs_next_level.push(f.clone());
                    }
                }

                return Some(CompactionJob {
                    input_level: i,
                    output_level: i + 1,
                    inputs_level: vec![file_to_compact],
                    inputs_next_level,
                });
            }
            level_max_bytes = level_max_bytes.saturating_mul(self.level_size_multiplier);
        }

        None
    }
}

pub struct Compactor {
    pub target_file_size: usize,
    pub work_dir: PathBuf,
    pub block_size: usize,
    pub bloom_bits_per_key: usize,
}

impl Compactor {
    /// Streams a `MergeIterator`, deduplicates versions, drops bottom-level tombstones, 
    /// and writes the output to new `SSTables`, returning a `VersionEdit`.
    #[allow(dead_code)]
    pub fn compact<I: StorageIterator>(
        &self,
        iter: MergeIterator<I>,
        next_file_id: &mut u64,
        output_level: usize,
        is_bottom_level: bool,
    ) -> std::io::Result<VersionEdit> {
        self.compact_with(
            iter,
            &mut || {
                let id = *next_file_id;
                *next_file_id += 1;
                id
            },
            output_level,
            is_bottom_level,
        )
    }

    pub fn compact_with<I: StorageIterator>(
        &self,
        mut iter: MergeIterator<I>,
        alloc_file_id: &mut dyn FnMut() -> u64,
        output_level: usize,
        is_bottom_level: bool,
    ) -> std::io::Result<VersionEdit> {
        let mut edit = VersionEdit::default();

        // State for the current SSTable being written
        let mut current_writer: Option<SSTableWriter<File>> = None;
        let mut current_file_id = 0;
        let mut min_key = Vec::new();
        let mut max_key = Vec::new();

        // State for MVCC deduplication
        let mut last_user_key = Vec::new();

        while iter.is_valid() {
            let internal_key = iter.key();
            if internal_key.len() < 9 {
                iter.next();
                continue;
            }

            let user_key = &internal_key[..internal_key.len() - 9];

            if user_key == last_user_key.as_slice() {
                iter.next();
                continue;
            }

            last_user_key.clear();
            last_user_key.extend_from_slice(user_key);

            let op_type = *internal_key.last().unwrap();
            if op_type == 0 && is_bottom_level {
                iter.next();
                continue;
            }

            if current_writer.is_none() {
                current_file_id = alloc_file_id();
                let path = self.work_dir.join(format!("{current_file_id:06}.sst"));
                let file = File::create(path)?;
                current_writer = Some(SSTableWriter::new(
                    file,
                    self.block_size,
                    16,
                    1,
                    self.bloom_bits_per_key,
                ));
                min_key = internal_key.to_vec();
            }

            current_writer
                .as_mut()
                .unwrap()
                .add(internal_key, iter.value())?;
            max_key = internal_key.to_vec();

            if current_writer.as_ref().unwrap().estimated_size() >= self.target_file_size {
                let sst_file = current_writer.take().unwrap().finish()?;
                sst_file.sync_all()?;
                let size = std::fs::metadata(self.work_dir.join(format!("{current_file_id:06}.sst")))?
                    .len();
                let meta = FileMetaData {
                    file_id: current_file_id,
                    file_size: size,
                    min_key: min_key.clone(),
                    max_key: max_key.clone(),
                };
                edit.add_file(output_level, meta);
            }

            iter.next();
        }

        if current_writer.is_some() {
            let sst_file = current_writer.take().unwrap().finish()?;
            sst_file.sync_all()?;
            let size = std::fs::metadata(self.work_dir.join(format!("{current_file_id:06}.sst")))?
                .len();
            let meta = FileMetaData {
                file_id: current_file_id,
                file_size: size,
                min_key,
                max_key,
            };
            edit.add_file(output_level, meta);
        }

        Ok(edit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::sstable::SsTable;
    use crate::memory::core::{encode_internal_key, ValueResult};

    struct TestIter {
        data: Vec<(Vec<u8>, Vec<u8>)>,
        index: usize,
    }

    impl TestIter {
        fn new(data: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
            Self { data, index: 0 }
        }
    }

    impl StorageIterator for TestIter {
        fn is_valid(&self) -> bool {
            self.index < self.data.len()
        }
        fn key(&self) -> &[u8] {
            &self.data[self.index].0
        }
        fn value(&self) -> &[u8] {
            &self.data[self.index].1
        }
        fn next(&mut self) {
            self.index += 1;
        }
    }

    #[test]
    fn test_compaction_deduplication_and_tombstone() {
        let temp_dir = std::env::temp_dir().join("lsm_test_compaction");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let compactor = Compactor {
            target_file_size: 4096,
            work_dir: temp_dir.clone(),
            block_size: 1024,
            bloom_bits_per_key: 10,
        };

        // Two versions of "key1": LSN 10 (newest) and LSN 5 (older)
        // One deleted key "key2": LSN 20 (op 0 delete) and older LSN 8 (op 1 put)
        // One normal key "key3": LSN 15 (op 1 put)
        let iter = TestIter::new(vec![
            (encode_internal_key(b"key1", 10, 1), b"val1_v2".to_vec()),
            (encode_internal_key(b"key1", 5, 1), b"val1_v1".to_vec()),
            (encode_internal_key(b"key2", 20, 0), b"".to_vec()),
            (encode_internal_key(b"key2", 8, 1), b"val2_old".to_vec()),
            (encode_internal_key(b"key3", 15, 1), b"val3".to_vec()),
        ]);

        let merge_iter = MergeIterator::new(vec![iter]);
        let mut next_file_id = 1;

        // Bottom level: tombstones incinerated!
        let edit = compactor
            .compact(merge_iter, &mut next_file_id, 1, true)
            .unwrap();

        assert_eq!(next_file_id, 2);
        assert_eq!(edit.new_files.len(), 1);

        let (level, meta) = &edit.new_files[0];
        assert_eq!(*level, 1);
        assert_eq!(meta.file_id, 1);

        // Verify the SSTable contents by reading it
        let sst_path = temp_dir.join(format!("{:06}.sst", meta.file_id));
        let sstable = SsTable::open(&sst_path).unwrap();

        // "key1" should return "val1_v2"
        assert_eq!(
            sstable.get(b"key1", 10).unwrap(),
            ValueResult::Found(b"val1_v2".to_vec())
        );
        // "key2" should be completely gone
        assert_eq!(sstable.get(b"key2", 20).unwrap(), ValueResult::NotFound);
        // "key3" should be present
        assert_eq!(
            sstable.get(b"key3", 15).unwrap(),
            ValueResult::Found(b"val3".to_vec())
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_compaction_non_bottom_keeps_tombstone() {
        let temp_dir = std::env::temp_dir().join("lsm_test_compaction_non_bottom");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let compactor = Compactor {
            target_file_size: 4096,
            work_dir: temp_dir.clone(),
            block_size: 1024,
            bloom_bits_per_key: 10,
        };

        // "key1" has tombstone at LSN 20, older version at LSN 5
        let iter = TestIter::new(vec![
            (encode_internal_key(b"key1", 20, 0), b"".to_vec()),
            (encode_internal_key(b"key1", 5, 1), b"old_val".to_vec()),
        ]);

        let merge_iter = MergeIterator::new(vec![iter]);
        let mut next_file_id = 1;

        // Non-bottom level: tombstone must NOT be dropped!
        let edit = compactor
            .compact(merge_iter, &mut next_file_id, 1, false)
            .unwrap();

        assert_eq!(edit.new_files.len(), 1);
        let sst_path = temp_dir.join(format!("{:06}.sst", edit.new_files[0].1.file_id));
        let sstable = SsTable::open(&sst_path).unwrap();

        // Reading at read_lsn 20 encounters the tombstone and returns ValueResult::Deleted (not older value)
        assert_eq!(sstable.get(b"key1", 20).unwrap(), ValueResult::Deleted);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_compaction_picker_l0_trigger() {
        let picker = CompactionPicker {
            l0_trigger: 4,
            base_level_size: 10 * 1024 * 1024,
            level_size_multiplier: 10,
        };

        let mut version = Version::new(7);
        // Add 3 files to L0: should not trigger
        for i in 1..=3 {
            version.files[0].push(FileMetaData {
                file_id: i,
                file_size: 1024,
                min_key: encode_internal_key(b"a", i, 1),
                max_key: encode_internal_key(b"b", i, 1),
            });
        }
        assert!(picker.pick_compaction(&version).is_none());

        // Add 4th file to L0: should trigger!
        version.files[0].push(FileMetaData {
            file_id: 4,
            file_size: 1024,
            min_key: encode_internal_key(b"c", 4, 1),
            max_key: encode_internal_key(b"d", 4, 1),
        });

        // Add overlapping and non-overlapping files to L1
        // L1 file 10: "b".."c" (overlaps with L0 range "a".."d")
        version.files[1].push(FileMetaData {
            file_id: 10,
            file_size: 2048,
            min_key: encode_internal_key(b"b", 1, 1),
            max_key: encode_internal_key(b"c", 1, 1),
        });
        // L1 file 11: "x".."z" (does NOT overlap with "a".."d")
        version.files[1].push(FileMetaData {
            file_id: 11,
            file_size: 2048,
            min_key: encode_internal_key(b"x", 1, 1),
            max_key: encode_internal_key(b"z", 1, 1),
        });

        let job = picker.pick_compaction(&version).unwrap();
        assert_eq!(job.input_level, 0);
        assert_eq!(job.output_level, 1);
        assert_eq!(job.inputs_level.len(), 4);
        assert_eq!(job.inputs_next_level.len(), 1);
        assert_eq!(job.inputs_next_level[0].file_id, 10);
    }
}
