use crossbeam_epoch::Guard;
use rand::RngExt;
use std::sync::atomic::Ordering;

struct SkipListNode {
    key: Vec<u8>,
    value: Vec<u8>,
    next: [crossbeam_epoch::Atomic<Self>; 32],
    height: usize,
}

impl SkipListNode {
    const fn new(key: Vec<u8>, value: Vec<u8>, height: usize) -> Self {
        // We INTEND to have the NULL_PTR copied 32 times.
        #[allow(clippy::declare_interior_mutable_const)]
        const NULL_PTR: crossbeam_epoch::Atomic<SkipListNode> = crossbeam_epoch::Atomic::null();
        Self {
            key,
            value,
            next: [NULL_PTR; 32],
            height,
        }
    }
}

pub struct SkipList {
    head: crossbeam_epoch::Atomic<SkipListNode>,
}

impl SkipList {
    pub fn new() -> Self {
        Self {
            head: crossbeam_epoch::Atomic::new(SkipListNode::new(Vec::new(), Vec::new(), 0)),
        }
    }

    pub fn get<'g>(&'g self, search_key: &[u8], guard: &'g Guard) -> Option<&'g [u8]> {
        let head_shared = self.head.load(Ordering::Acquire, guard);

        // SAFETY: The head node is instantiated on creation and never dropped or mutated, simply a dummy.
        let mut current_node = unsafe { head_shared.as_ref().expect("head should be present") };

        for level in (0..32).rev() {
            loop {
                let next_shared = current_node.next[level].load(Ordering::Acquire, guard);
                if next_shared.is_null() {
                    break;
                }

                // SAFETY: The next node must exist since it is not null.
                let next_node = unsafe { next_shared.as_ref().expect("next is not null") };
                match next_node.key.as_slice().cmp(search_key) {
                    std::cmp::Ordering::Equal => return Some(next_node.value.as_slice()),
                    std::cmp::Ordering::Less => current_node = next_node,
                    std::cmp::Ordering::Greater => break,
                }
            }
        }

        let next_shared = current_node.next[0].load(Ordering::Acquire, guard);
        if next_shared.is_null() {
            return None;
        }

        // SAFETY: The next node must exist since it is not null.
        let next_node = unsafe { next_shared.as_ref().expect("next is not null") };
        if next_node.key.as_slice() == search_key {
            Some(next_node.value.as_slice())
        } else {
            None
        }
    }

    pub fn put<'g>(&'g self, key: Vec<u8>, value: Vec<u8>, guard: &'g Guard) {
        let mut update: [crossbeam_epoch::Shared<SkipListNode>; 32] =
            [crossbeam_epoch::Shared::null(); 32];

        let mut current_shared = self.head.load(Ordering::Acquire, guard);

        for level in (0..32).rev() {
            loop {
                // SAFETY: Dummy head or previously visited node is valid
                let current_node = unsafe { current_shared.as_ref().expect("node exists") };

                let next_shared = current_node.next[level].load(Ordering::Acquire, guard);
                if next_shared.is_null() {
                    break;
                }

                let next_node = unsafe { next_shared.as_ref().unwrap() };
                if next_node.key.as_slice() < key.as_slice() {
                    current_shared = next_shared;
                } else {
                    break;
                }
            }
            update[level] = current_shared;
        }

        let r = rand::rng().random::<u32>();
        let zeros = r.trailing_zeros();
        let height = (zeros + 1).min(32) as usize;
        let new_node =
            crossbeam_epoch::Owned::new(SkipListNode::new(key, value, height)).into_shared(guard);
        let my_key = unsafe { new_node.as_ref().unwrap() }.key.as_slice();

        #[allow(clippy::needless_range_loop)]
        for level in 0..height {
            let mut prev_node = unsafe { update[level].as_ref().unwrap() };
            let mut next_shared = prev_node.next[level].load(Ordering::Acquire, guard);

            loop {
                unsafe { new_node.as_ref().unwrap() }.next[level]
                    .store(next_shared, Ordering::Relaxed);
                match prev_node.next[level].compare_exchange(
                    next_shared,
                    new_node,
                    Ordering::Release,
                    Ordering::Relaxed,
                    guard,
                ) {
                    Ok(_) => break,
                    Err(err) => {
                        let actual_node = unsafe { err.current.as_ref().unwrap() };

                        if actual_node.key.as_slice() >= my_key {
                            next_shared = err.current;
                        } else {
                            prev_node = actual_node;
                            loop {
                                next_shared = prev_node.next[level].load(Ordering::Acquire, guard);
                                if next_shared.is_null() {
                                    break;
                                }

                                let next_node = unsafe { next_shared.as_ref().unwrap() };
                                if next_node.key.as_slice() < my_key {
                                    prev_node = next_node;
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_epoch::pin;

    #[test]
    fn test_skiplist_basic_operations() {
        let list = SkipList::new();

        let guard = pin();

        list.put(b"zebra".to_vec(), b"stripes".to_vec(), &guard);
        list.put(b"apple".to_vec(), b"red".to_vec(), &guard);
        list.put(b"monkey".to_vec(), b"banana".to_vec(), &guard);

        assert_eq!(list.get(b"apple", &guard), Some(b"red".as_slice()));
        assert_eq!(list.get(b"zebra", &guard), Some(b"stripes".as_slice()));
        assert_eq!(list.get(b"monkey", &guard), Some(b"banana".as_slice()));

        assert_eq!(list.get(b"ghost", &guard), None);

        list.put(b"aardvark".to_vec(), b"ant".to_vec(), &guard);
        list.put(b"zulu".to_vec(), b"end".to_vec(), &guard);

        assert_eq!(list.get(b"aardvark", &guard), Some(b"ant".as_slice()));
        assert_eq!(list.get(b"zulu", &guard), Some(b"end".as_slice()));
    }
}
