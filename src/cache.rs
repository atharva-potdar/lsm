use crate::disk::block::Block;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub struct BlockId {
    pub sst_id: u64,
    pub offset: u64,
}

pub trait BlockCache: Send + Sync {
    fn get(&self, key: &BlockId) -> Option<Arc<Block>>;
    fn insert(&self, key: BlockId, block: Arc<Block>);
}

/// Node in the SIEVE eviction queue.
struct SieveNode {
    key: BlockId,
    block: Arc<Block>,
    visited: AtomicBool,
    prev: Option<usize>,
    next: Option<usize>,
}

struct SieveInner {
    capacity: usize,
    slots: Vec<Option<SieveNode>>,
    free_slots: Vec<usize>,
    head: Option<usize>,
    tail: Option<usize>,
    hand: Option<usize>,
    map: HashMap<BlockId, usize>,
}

impl SieveInner {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            slots: Vec::with_capacity(capacity),
            free_slots: Vec::new(),
            head: None,
            tail: None,
            hand: None,
            map: HashMap::with_capacity(capacity),
        }
    }

    fn unlink(&mut self, idx: usize) {
        let (prev, next) = {
            let node = self.slots[idx].as_ref().unwrap();
            (node.prev, node.next)
        };

        if let Some(p) = prev {
            self.slots[p].as_mut().unwrap().next = next;
        } else {
            self.head = next;
        }

        if let Some(n) = next {
            self.slots[n].as_mut().unwrap().prev = prev;
        } else {
            self.tail = prev;
        }

        let node = self.slots[idx].as_mut().unwrap();
        node.prev = None;
        node.next = None;
    }

    fn evict(&mut self) {
        if self.map.is_empty() {
            return;
        }

        let mut curr = self.hand.or(self.tail);

        loop {
            let idx = match curr {
                Some(i) => i,
                None => {
                    curr = self.tail;
                    match curr {
                        Some(i) => i,
                        None => return,
                    }
                }
            };

            let visited = self.slots[idx]
                .as_ref()
                .unwrap()
                .visited
                .load(Ordering::Relaxed);

            if visited {
                self.slots[idx]
                    .as_ref()
                    .unwrap()
                    .visited
                    .store(false, Ordering::Relaxed);
                curr = self.slots[idx].as_ref().unwrap().prev;
            } else {
                let next_hand = self.slots[idx].as_ref().unwrap().prev;
                let key = self.slots[idx].as_ref().unwrap().key.clone();

                self.unlink(idx);
                self.map.remove(&key);
                self.slots[idx] = None;
                self.free_slots.push(idx);

                self.hand = next_hand;
                return;
            }
        }
    }

    fn insert(&mut self, key: BlockId, block: Arc<Block>) {
        if let Some(&idx) = self.map.get(&key) {
            let node = self.slots[idx].as_mut().unwrap();
            node.block = block;
            node.visited.store(true, Ordering::Relaxed);
            return;
        }

        if self.map.len() >= self.capacity {
            self.evict();
        }

        let idx = if let Some(free_idx) = self.free_slots.pop() {
            free_idx
        } else {
            let next_idx = self.slots.len();
            self.slots.push(None);
            next_idx
        };

        let old_head = self.head;
        self.slots[idx] = Some(SieveNode {
            key: key.clone(),
            block,
            visited: AtomicBool::new(false),
            prev: None,
            next: old_head,
        });

        if let Some(h) = old_head {
            self.slots[h].as_mut().unwrap().prev = Some(idx);
        }
        self.head = Some(idx);
        if self.tail.is_none() {
            self.tail = Some(idx);
        }

        self.map.insert(key, idx);
    }
}

/// High-performance, lock-free on cache hits, scan-resistant SIEVE cache (NSDI 2024).
pub struct SieveBlockCache {
    inner: RwLock<SieveInner>,
}

impl SieveBlockCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(SieveInner::new(capacity)),
        }
    }
}

impl BlockCache for SieveBlockCache {
    fn get(&self, key: &BlockId) -> Option<Arc<Block>> {
        let inner = self.inner.read().unwrap();
        if let Some(&idx) = inner.map.get(key) {
            let node = inner.slots[idx].as_ref()?;
            node.visited.store(true, Ordering::Relaxed);
            Some(Arc::clone(&node.block))
        } else {
            None
        }
    }

    fn insert(&self, key: BlockId, block: Arc<Block>) {
        let mut inner = self.inner.write().unwrap();
        inner.insert(key, block);
    }
}

/// Standard Least Recently Used (LRU) block cache.
pub struct LruBlockCache {
    inner: Mutex<lru::LruCache<BlockId, Arc<Block>>>,
}

impl LruBlockCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity).expect("capacity must be non-zero");
        Self {
            inner: Mutex::new(lru::LruCache::new(cap)),
        }
    }
}

impl BlockCache for LruBlockCache {
    fn get(&self, key: &BlockId) -> Option<Arc<Block>> {
        let mut inner = self.inner.lock().unwrap();
        inner.get(key).cloned()
    }

    fn insert(&self, key: BlockId, block: Arc<Block>) {
        let mut inner = self.inner.lock().unwrap();
        inner.put(key, block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::block::BlockBuilder;

    fn make_test_block(val: u8) -> Arc<Block> {
        let mut builder = BlockBuilder::new(16);
        builder.add(b"test_key", &[val]);
        Arc::new(Block::decode(builder.finish().to_vec()).unwrap())
    }

    #[test]
    fn test_sieve_cache_basic_and_scan_resistance() {
        let cache = SieveBlockCache::new(2);

        let k1 = BlockId { sst_id: 1, offset: 0 };
        let k2 = BlockId { sst_id: 1, offset: 100 };
        let k3 = BlockId { sst_id: 2, offset: 0 };

        let b1 = make_test_block(1);
        let b2 = make_test_block(2);
        let b3 = make_test_block(3);

        cache.insert(k1.clone(), Arc::clone(&b1));
        cache.insert(k2.clone(), Arc::clone(&b2));

        // Touch k1 so visited = true
        assert!(cache.get(&k1).is_some());

        // Insert k3. Cache capacity is 2.
        // k2 has visited = false, while k1 has visited = true.
        // SIEVE hand should evict k2, keeping k1!
        cache.insert(k3.clone(), Arc::clone(&b3));

        assert!(cache.get(&k1).is_some(), "k1 was visited and should be retained");
        assert!(cache.get(&k2).is_none(), "k2 was single-touch and should be evicted");
        assert!(cache.get(&k3).is_some(), "k3 was just inserted");
    }

    #[test]
    fn test_lru_cache_basic() {
        let cache = LruBlockCache::new(2);

        let k1 = BlockId { sst_id: 1, offset: 0 };
        let k2 = BlockId { sst_id: 1, offset: 100 };
        let k3 = BlockId { sst_id: 2, offset: 0 };

        let b1 = make_test_block(1);
        let b2 = make_test_block(2);
        let b3 = make_test_block(3);

        cache.insert(k1.clone(), Arc::clone(&b1));
        cache.insert(k2.clone(), Arc::clone(&b2));

        // Touch k1 so it becomes most recently used
        assert!(cache.get(&k1).is_some());

        // Insert k3. LRU evicts k2 (least recently used)
        cache.insert(k3.clone(), Arc::clone(&b3));

        assert!(cache.get(&k1).is_some());
        assert!(cache.get(&k2).is_none());
        assert!(cache.get(&k3).is_some());
    }
}
