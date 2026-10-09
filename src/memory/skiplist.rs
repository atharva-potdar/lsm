use crossbeam_epoch::Guard;
use std::sync::atomic::Ordering;

struct SkipListNode {
    key: Vec<u8>,
    value: Vec<u8>,
    next: [crossbeam_epoch::Atomic<SkipListNode>; 32],
    height: usize,
}

impl SkipListNode {
    pub fn new(key: Vec<u8>, value: Vec<u8>, height: usize) -> Self {
        const NULL_PTR: crossbeam_epoch::Atomic<SkipListNode> = crossbeam_epoch::Atomic::null();
        Self {
            key,
            value,
            next: [NULL_PTR; 32],
            height,
        }
    }
}

struct SkipList {
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
                if next_node.key.as_slice() < search_key {
                    current_node = next_node;
                } else {
                    break;
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
}