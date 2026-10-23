#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::significant_drop_tightening
)]

use crate::disk::manifest::{FileMetaData, VersionEdit, VersionSet};
use crate::disk::sstable::{SSTableWriter, SsTable};
use crate::disk::wal::{WalReader, WalWriter};
use crate::memory::core::{encode_internal_key, ValueResult};
use crate::memory::memtable::MemTable;
use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

pub struct ColdState {
    pub immutables: Vec<Arc<MemTable>>,
    pub versions: VersionSet,
    pub sstables: HashMap<u64, Arc<SsTable>>, // Cache of open SSTables
}

pub struct DB {
    pub work_dir: PathBuf,
    pub next_lsn: AtomicU64,
    pub memtable: RwLock<Arc<MemTable>>,
    pub wal: Mutex<WalWriter>,
    pub state: Mutex<ColdState>,
    pub memtable_capacity: usize,
}

impl DB {
    /// Opens the database, recovers from WAL if it exists, and initializes the `VersionSet`.
    pub fn open(work_dir: impl Into<PathBuf>, memtable_capacity: usize) -> std::io::Result<Arc<Self>> {
        let work_dir = work_dir.into();
        std::fs::create_dir_all(&work_dir)?;

        let wal_path = work_dir.join("wal.log");
        let memtable = MemTable::new();
        let mut next_lsn = 1;

        if wal_path.exists() {
            let mut reader = WalReader::new(&wal_path)?;
            while let Some((internal_key, value)) = reader.next()? {
                if internal_key.len() < 9 {
                    continue;
                }
                let user_key = &internal_key[..internal_key.len() - 9];
                let lsn_bytes: [u8; 8] = internal_key[internal_key.len() - 9..internal_key.len() - 1]
                    .try_into()
                    .unwrap();
                let lsn = u64::MAX - u64::from_be_bytes(lsn_bytes);
                let op_type = *internal_key.last().unwrap();

                next_lsn = next_lsn.max(lsn + 1);
                if op_type == 1 {
                    memtable.put(user_key, &value, lsn);
                } else if op_type == 0 {
                    memtable.delete(user_key, lsn);
                }
            }
        }

        let wal = WalWriter::new(&wal_path)?;

        let cold_state = ColdState {
            immutables: Vec::new(),
            versions: VersionSet::new(7), // 7 levels
            sstables: HashMap::new(),
        };

        Ok(Arc::new(Self {
            work_dir,
            next_lsn: AtomicU64::new(next_lsn),
            memtable: RwLock::new(Arc::new(memtable)),
            wal: Mutex::new(wal),
            state: Mutex::new(cold_state),
            memtable_capacity,
        }))
    }

    pub fn put(&self, key: &[u8], value: &[u8]) -> std::io::Result<()> {
        let lsn = self.next_lsn.fetch_add(1, Ordering::SeqCst);
        let internal_key = encode_internal_key(key, lsn, 1);

        self.wal.lock().unwrap().append(&internal_key, value)?;

        let needs_flush = {
            let memtable = self.memtable.read().unwrap();
            memtable.put(key, value, lsn);
            memtable.approximate_size() >= self.memtable_capacity
        };

        if needs_flush {
            self.force_flush()?;
        }

        Ok(())
    }

    pub fn delete(&self, key: &[u8]) -> std::io::Result<()> {
        let lsn = self.next_lsn.fetch_add(1, Ordering::SeqCst);
        let internal_key = encode_internal_key(key, lsn, 0);

        self.wal.lock().unwrap().append(&internal_key, b"")?;

        let needs_flush = {
            let memtable = self.memtable.read().unwrap();
            memtable.delete(key, lsn);
            memtable.approximate_size() >= self.memtable_capacity
        };

        if needs_flush {
            self.force_flush()?;
        }

        Ok(())
    }

    pub fn get(&self, key: &[u8]) -> std::io::Result<Option<Vec<u8>>> {
        let read_lsn = self.next_lsn.load(Ordering::SeqCst);

        let guard = crossbeam_epoch::pin();
        {
            let memtable = self.memtable.read().unwrap();
            match memtable.get(key, &guard) {
                ValueResult::Found(val) => return Ok(Some(val)),
                ValueResult::Deleted => return Ok(None),
                ValueResult::NotFound => {}
            }
        }

        let mut state = self.state.lock().unwrap();

        for immutable in state.immutables.iter().rev() {
            match immutable.get(key, &guard) {
                ValueResult::Found(val) => return Ok(Some(val)),
                ValueResult::Deleted => return Ok(None),
                ValueResult::NotFound => {}
            }
        }

        // Level 0: newest files are at the end, so check in reverse
        for meta in state.versions.current.files[0].clone().iter().rev() {
            let file_id = meta.file_id;
            let table = if let Some(table) = state.sstables.get(&file_id) {
                Arc::clone(table)
            } else {
                let path = self.work_dir.join(format!("{file_id:06}.sst"));
                let table = Arc::new(SsTable::open(&path)?);
                state.sstables.insert(file_id, Arc::clone(&table));
                table
            };

            match table.get(key, read_lsn)? {
                ValueResult::Found(val) => return Ok(Some(val)),
                ValueResult::Deleted => return Ok(None),
                ValueResult::NotFound => {}
            }
        }

        // Levels 1 to 6
        for level in 1..state.versions.current.files.len() {
            let files = &state.versions.current.files[level];
            if files.is_empty() {
                continue;
            }

            let idx = files.partition_point(|meta| {
                let max_u = if meta.max_key.len() >= 9 {
                    &meta.max_key[..meta.max_key.len() - 9]
                } else {
                    &meta.max_key[..]
                };
                max_u < key
            });

            if idx < files.len() {
                let meta = &files[idx];
                let min_u = if meta.min_key.len() >= 9 {
                    &meta.min_key[..meta.min_key.len() - 9]
                } else {
                    &meta.min_key[..]
                };

                if min_u <= key {
                    let file_id = meta.file_id;
                    let table = if let Some(table) = state.sstables.get(&file_id) {
                        Arc::clone(table)
                    } else {
                        let path = self.work_dir.join(format!("{file_id:06}.sst"));
                        let table = Arc::new(SsTable::open(&path)?);
                        state.sstables.insert(file_id, Arc::clone(&table));
                        table
                    };

                    match table.get(key, read_lsn)? {
                        ValueResult::Found(val) => return Ok(Some(val)),
                        ValueResult::Deleted => return Ok(None),
                        ValueResult::NotFound => {}
                    }
                }
            }
        }

        Ok(None)
    }

    /// Synchronously flushes the active `MemTable` to a Level 0 `SSTable`.
    pub fn force_flush(&self) -> std::io::Result<()> {
        let old_memtable = {
            let mut active = self.memtable.write().unwrap();
            std::mem::replace(&mut *active, Arc::new(MemTable::new()))
        };

        {
            let mut state = self.state.lock().unwrap();
            state.immutables.push(Arc::clone(&old_memtable));
        }

        let file_id = {
            let mut state = self.state.lock().unwrap();
            state.versions.new_file_id()
        };

        let sst_path = self.work_dir.join(format!("{file_id:06}.sst"));
        let file = File::create(&sst_path)?;
        let mut writer = SSTableWriter::new(file, 4096, 16, 1, 10);

        let guard = crossbeam_epoch::pin();
        let mut min_key = Vec::new();
        let mut max_key = Vec::new();
        let mut count = 0;

        for (k, v) in old_memtable.iter(&guard) {
            if count == 0 {
                min_key = k.to_vec();
            }
            max_key = k.to_vec();
            writer.add(k, v)?;
            count += 1;
        }

        if count > 0 {
            let sst_file = writer.finish()?;
            sst_file.sync_all()?;
            let file_size = std::fs::metadata(&sst_path)?.len();
            let meta = FileMetaData {
                file_id,
                file_size,
                min_key,
                max_key,
            };
            let mut edit = VersionEdit::default();
            edit.add_file(0, meta);

            let mut state = self.state.lock().unwrap();
            state.versions.log_and_apply(&edit);
            state.immutables.retain(|m| !Arc::ptr_eq(m, &old_memtable));
        } else {
            let _ = std::fs::remove_file(&sst_path);
            let mut state = self.state.lock().unwrap();
            state.immutables.retain(|m| !Arc::ptr_eq(m, &old_memtable));
        }

        // Only after the SSTable has been written and committed do we delete the old WAL.
        let wal_path = self.work_dir.join("wal.log");
        {
            let mut wal = self.wal.lock().unwrap();
            let _ = std::fs::remove_file(&wal_path);
            *wal = WalWriter::new(&wal_path)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_put_get() {
        let temp_dir = std::env::temp_dir().join("lsm_test_db_put_get");
        let _ = std::fs::remove_dir_all(&temp_dir);

        let db = DB::open(&temp_dir, 1024 * 1024).unwrap();
        db.put(b"foo", b"bar").unwrap();
        db.put(b"hello", b"world").unwrap();

        assert_eq!(db.get(b"foo").unwrap(), Some(b"bar".to_vec()));
        assert_eq!(db.get(b"hello").unwrap(), Some(b"world".to_vec()));
        assert_eq!(db.get(b"missing").unwrap(), None);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_db_force_flush() {
        let temp_dir = std::env::temp_dir().join("lsm_test_db_force_flush");
        let _ = std::fs::remove_dir_all(&temp_dir);

        let db = DB::open(&temp_dir, 1024 * 1024).unwrap();
        db.put(b"key1", b"val1").unwrap();
        db.put(b"key2", b"val2").unwrap();

        db.force_flush().unwrap();

        // Active memtable should be empty, data now resides in Level 0 SSTable
        assert_eq!(db.get(b"key1").unwrap(), Some(b"val1".to_vec()));
        assert_eq!(db.get(b"key2").unwrap(), Some(b"val2".to_vec()));

        // Add new key into fresh memtable
        db.put(b"key3", b"val3").unwrap();
        assert_eq!(db.get(b"key3").unwrap(), Some(b"val3".to_vec()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_db_recovery_from_wal() {
        let temp_dir = std::env::temp_dir().join("lsm_test_db_wal_recovery");
        let _ = std::fs::remove_dir_all(&temp_dir);

        {
            let db = DB::open(&temp_dir, 1024 * 1024).unwrap();
            db.put(b"recovered_key1", b"val1").unwrap();
            db.put(b"recovered_key2", b"val2").unwrap();
            // Drop without force_flush!
        }

        // Reopen DB from the same directory
        let db = DB::open(&temp_dir, 1024 * 1024).unwrap();
        assert_eq!(db.get(b"recovered_key1").unwrap(), Some(b"val1".to_vec()));
        assert_eq!(db.get(b"recovered_key2").unwrap(), Some(b"val2".to_vec()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_db_auto_flush_capacity() {
        let temp_dir = std::env::temp_dir().join("lsm_test_db_auto_flush");
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Small capacity to trigger auto flush
        let db = DB::open(&temp_dir, 20).unwrap();
        db.put(b"long_key_01", b"long_val_01").unwrap();
        db.put(b"long_key_02", b"long_val_02").unwrap();

        assert_eq!(db.get(b"long_key_01").unwrap(), Some(b"long_val_01".to_vec()));
        assert_eq!(db.get(b"long_key_02").unwrap(), Some(b"long_val_02".to_vec()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_db_tombstone_resurrection_bug_fix() {
        let temp_dir = std::env::temp_dir().join("lsm_test_db_tombstone_resurrection");
        let _ = std::fs::remove_dir_all(&temp_dir);

        let db = DB::open(&temp_dir, 1024 * 1024).unwrap();

        // 1. Insert ("apple", "v1")
        db.put(b"apple", b"v1").unwrap();

        // 2. Delete "apple"
        db.delete(b"apple").unwrap();

        // 3. Force flush
        db.force_flush().unwrap();

        // 4. Before the fix, this returned Some("v1"). After the fix, it must return None.
        assert_eq!(db.get(b"apple").unwrap(), None);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_db_tombstone_in_memtable_shadows_disk() {
        let temp_dir = std::env::temp_dir().join("lsm_test_db_tombstone_shadows_disk");
        let _ = std::fs::remove_dir_all(&temp_dir);

        let db = DB::open(&temp_dir, 1024 * 1024).unwrap();

        // Put and flush to disk
        db.put(b"apple", b"v1").unwrap();
        db.force_flush().unwrap();
        assert_eq!(db.get(b"apple").unwrap(), Some(b"v1".to_vec()));

        // Delete in memtable without flush
        db.delete(b"apple").unwrap();

        // Must return None (tombstone in active memtable masks L0 SSTable on disk)
        assert_eq!(db.get(b"apple").unwrap(), None);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_db_prefix_lsn_ordering() {
        let temp_dir = std::env::temp_dir().join("lsm_test_db_prefix_lsn");
        let _ = std::fs::remove_dir_all(&temp_dir);

        let db = DB::open(&temp_dir, 1024 * 1024).unwrap();

        // Key A is a prefix of Key B
        db.put(&[0x01], b"prefix_val").unwrap();
        db.put(&[0x01, 0x02], b"longer_val").unwrap();

        assert_eq!(db.get(&[0x01]).unwrap(), Some(b"prefix_val".to_vec()));
        assert_eq!(db.get(&[0x01, 0x02]).unwrap(), Some(b"longer_val".to_vec()));

        db.force_flush().unwrap();

        assert_eq!(db.get(&[0x01]).unwrap(), Some(b"prefix_val".to_vec()));
        assert_eq!(db.get(&[0x01, 0x02]).unwrap(), Some(b"longer_val".to_vec()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
