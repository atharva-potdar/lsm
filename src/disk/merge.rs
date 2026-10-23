use crate::memory::core::cmp_internal_key;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// A unified trait for anything that can yield Key-Value pairs in sorted order.
/// This allows us to merge `MemTables`, Blocks, and `SSTables` using the same code.
pub trait StorageIterator {
    fn is_valid(&self) -> bool;
    fn key(&self) -> &[u8];
    fn value(&self) -> &[u8];
    fn next(&mut self);
}

/// A wrapper to put iterators into a `BinaryHeap`.
pub struct HeapWrapper<I: StorageIterator> {
    pub iter: I,
    pub index: usize, // Tie-breaker: lower index = newer data
}

impl<I: StorageIterator> PartialEq for HeapWrapper<I> {
    fn eq(&self, other: &Self) -> bool {
        self.iter.key() == other.iter.key() && self.index == other.index
    }
}

impl<I: StorageIterator> Eq for HeapWrapper<I> {}

impl<I: StorageIterator> PartialOrd for HeapWrapper<I> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<I: StorageIterator> Ord for HeapWrapper<I> {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse the comparison of keys to turn Rust's Max-Heap into a Min-Heap!
        // Also reverse the comparison of indices so lower index (newer data) has higher priority!
        cmp_internal_key(other.iter.key(), self.iter.key())
            .then_with(|| other.index.cmp(&self.index))
    }
}

pub struct MergeIterator<I: StorageIterator> {
    heap: BinaryHeap<HeapWrapper<I>>,
    current: Option<HeapWrapper<I>>,
}

impl<I: StorageIterator> MergeIterator<I> {
    pub fn new(iters: Vec<I>) -> Self {
        let mut heap = BinaryHeap::new();
        for (index, iter) in iters.into_iter().enumerate() {
            if iter.is_valid() {
                heap.push(HeapWrapper { iter, index });
            }
        }
        let current = heap.pop();
        Self { heap, current }
    }
}

impl<I: StorageIterator> StorageIterator for MergeIterator<I> {
    fn is_valid(&self) -> bool {
        self.current.is_some()
    }

    fn key(&self) -> &[u8] {
        self.current.as_ref().unwrap().iter.key()
    }

    fn value(&self) -> &[u8] {
        self.current.as_ref().unwrap().iter.value()
    }

    fn next(&mut self) {
        if let Some(mut wrapper) = self.current.take() {
            wrapper.iter.next();
            if wrapper.iter.is_valid() {
                self.heap.push(wrapper);
            }
            self.current = self.heap.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockIterator {
        data: Vec<(Vec<u8>, Vec<u8>)>,
        index: usize,
    }

    impl MockIterator {
        fn new(data: Vec<(&'static str, &'static str)>) -> Self {
            let data = data
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
                .collect();
            Self { data, index: 0 }
        }
    }

    impl StorageIterator for MockIterator {
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
    fn test_merge_iterator() {
        let iter1 = MockIterator::new(vec![("a", "1"), ("c", "3"), ("e", "5")]);
        let iter2 = MockIterator::new(vec![("b", "2"), ("c", "3.5"), ("d", "4")]);
        let iter3 = MockIterator::new(vec![("f", "6")]);

        let mut merge_iter = MergeIterator::new(vec![iter1, iter2, iter3]);

        let mut results = Vec::new();
        while merge_iter.is_valid() {
            results.push((
                String::from_utf8(merge_iter.key().to_vec()).unwrap(),
                String::from_utf8(merge_iter.value().to_vec()).unwrap(),
            ));
            merge_iter.next();
        }

        assert_eq!(
            results,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
                // iter1 has index 0, iter2 has index 1. iter1 wins the tie-breaker!
                ("c".to_string(), "3".to_string()), // Wait, iter1 had ("c", "3"), so "3" wins!
                ("c".to_string(), "3.5".to_string()),
                ("d".to_string(), "4".to_string()),
                ("e".to_string(), "5".to_string()),
                ("f".to_string(), "6".to_string()),
            ]
        );
    }
}
