use crate::disk::manifest::{FileMetaData, VersionEdit};
use crate::disk::merge::{MergeIterator, StorageIterator};
use crate::disk::sstable::SSTableWriter;
use std::fs::File;
use std::path::PathBuf;

pub struct Compactor {
    pub target_file_size: usize,
    pub work_dir: PathBuf,
    pub block_size: usize,
    pub bloom_bits_per_key: usize,
}

impl Compactor {
    /// Streams a `MergeIterator`, deduplicates versions, drops bottom-level tombstones, 
    /// and writes the output to new `SSTables`, returning a `VersionEdit`.
    pub fn compact<I: StorageIterator>(
        &self,
        mut iter: MergeIterator<I>,
        next_file_id: &mut u64,
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
                current_file_id = *next_file_id;
                *next_file_id += 1;
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
                current_writer.take().unwrap().finish()?;
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
            current_writer.take().unwrap().finish()?;
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
}
