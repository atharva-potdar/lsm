use crate::memory::core::encode_internal_key;
use crate::memory::skiplist;
use crossbeam_epoch::Guard;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct MemTable {
    skip_list: skiplist::SkipList,
    approximate_size: AtomicUsize,
}

impl MemTable {
    pub fn new() -> Self {
        Self {
            skip_list: skiplist::SkipList::new(),
            approximate_size: AtomicUsize::new(0),
        }
    }

    pub fn put(&self, key: &[u8], value: &[u8], lsn: u64) {
        let added_size = key.len() + value.len();
        self.approximate_size
            .fetch_add(added_size, Ordering::Relaxed);
        let guard = crossbeam_epoch::pin();
        let internal_key = encode_internal_key(key, lsn, 1);
        self.skip_list.put(internal_key, value.to_vec(), &guard);
    }

    pub fn get<'g>(&'g self, user_key: &[u8], guard: &'g Guard) -> Option<&'g [u8]> {
        let (found_key, found_value) = self.skip_list.seek(user_key, guard)?;

        if !found_key.starts_with(user_key) {
            return None;
        }

        let op_type = *found_key.last().unwrap();

        if op_type == 1 {
            Some(found_value)
        } else {
            None
        }
    }

    pub fn delete(&self, key: &[u8], lsn: u64) {
        let added_size = key.len();
        self.approximate_size
            .fetch_add(added_size, Ordering::Relaxed);

        let internal_key = encode_internal_key(key, lsn, 0);
        let guard = crossbeam_epoch::pin();

        self.skip_list.put(internal_key, Vec::new(), &guard);
    }

    pub fn approximate_size(&self) -> usize {
        self.approximate_size.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_epoch::pin;

    #[test]
    fn test_memtable_mvcc_and_tombstones() {
        let memtable = MemTable::new();
        let guard = pin();

        memtable.put(b"apple", b"red", 1);
        assert_eq!(memtable.get(b"apple", &guard), Some(b"red".as_slice()));

        memtable.put(b"apple", b"green", 2);
        assert_eq!(memtable.get(b"apple", &guard), Some(b"green".as_slice()));

        memtable.put(b"banana", b"yellow", 3);
        assert_eq!(memtable.get(b"banana", &guard), Some(b"yellow".as_slice()));

        memtable.delete(b"apple", 4);
        assert_eq!(memtable.get(b"apple", &guard), None);
        assert_eq!(memtable.get(b"banana", &guard), Some(b"yellow".as_slice()));
        assert_eq!(memtable.approximate_size(), 35);
    }
}
