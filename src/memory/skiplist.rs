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
