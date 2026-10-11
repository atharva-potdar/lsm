#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::cmp::Ordering;

pub struct BlockBuilder {
    buffer: Vec<u8>,
    restart_points: Vec<u32>,
    restart_interval: usize,
    counter: usize,
    last_key: Vec<u8>,
}

impl BlockBuilder {
    fn varint_push(&mut self, mut val: u32) {
        loop {
            let mut byte = (val & 0x7F) as u8;
            val >>= 7;
            if val != 0 {
                byte |= 0x80;
            }
            self.buffer.push(byte);
            if val == 0 {
                break;
            }
        }
    }
    pub fn new(restart_interval: usize) -> Self {
        Self {
            buffer: Vec::new(),
            restart_points: vec![0],
            restart_interval,
            counter: 0,
            last_key: Vec::new(),
        }
    }
    pub fn add(&mut self, key: &[u8], value: &[u8]) {
        let shared_len;

        if self.counter >= self.restart_interval {
            shared_len = 0;
            self.restart_points.push(self.buffer.len() as u32);
            self.counter = 0;
        } else {
            shared_len = self
                .last_key
                .iter()
                .zip(key.iter())
                .take_while(|(a, b)| a == b)
                .count();
        }

        let unshared_len = key.len() - shared_len;

        self.varint_push(shared_len as u32);
        self.varint_push(unshared_len as u32);
        self.varint_push(value.len() as u32);
        self.buffer.extend_from_slice(&key[shared_len..]);
        self.buffer.extend_from_slice(value);

        self.last_key.truncate(shared_len);
        self.last_key.extend_from_slice(&key[shared_len..]);
        self.counter += 1;
    }

    pub const fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn finish(&mut self) -> &[u8] {
        for offset in &self.restart_points {
            let offset_bytes: [u8; 4] = offset.to_le_bytes();
            self.buffer.extend_from_slice(&offset_bytes);
        }
        let num_restarts = self.restart_points.len() as u32;
        let num_restarts_bytes: [u8; 4] = num_restarts.to_le_bytes();
        self.buffer.extend_from_slice(&num_restarts_bytes);

        &self.buffer
    }

    pub const fn estimated_size(&self) -> usize {
        self.buffer.len() + self.restart_points.len() * 4 + 4
    }
}

pub struct Block {
    data: Vec<u8>,
    restart_offset: usize,
    num_restarts: usize,
}

impl Block {
    const fn decode_varint(data: &[u8], mut offset: usize) -> (u32, usize) {
        let mut result: u32 = 0;
        let mut shift = 0;

        loop {
            if offset >= data.len() {
                break;
            }

            let byte = data[offset];
            offset += 1;
            let val = byte & 0x7F;
            result |= (val as u32) << shift;

            // shift can never exceed 28 for an u32
            if (byte & 0x80) == 0 || shift >= 28 {
                break;
            }
            shift += 7;
        }

        (result, offset)
    }

    pub fn new(data: Vec<u8>) -> Self {
        let num_restarts = u32::from_le_bytes(data[data.len() - 4..].try_into().unwrap()) as usize;
        let restart_offset = data.len() - 4 - num_restarts * 4;
        Self {
            data,
            restart_offset,
            num_restarts,
        }
    }
}

pub struct BlockIterator<'a> {
    block: &'a Block,
    offset: usize,
    next_offset: usize,
    key: Vec<u8>,
    value: &'a [u8],
}

impl<'a> BlockIterator<'a> {
    pub const fn new(block: &'a Block) -> Self {
        Self {
            block,
            offset: block.restart_offset,
            next_offset: block.restart_offset,
            key: Vec::new(),
            value: &[],
        }
    }

    pub const fn is_valid(&self) -> bool {
        self.offset < self.block.restart_offset
    }

    fn decode_entry_at(&mut self, offset: usize) {
        let (shared_len, offset1) = Block::decode_varint(&self.block.data, offset);
        let (unshared_len, offset2) = Block::decode_varint(&self.block.data, offset1);
        let (value_len, offset3) = Block::decode_varint(&self.block.data, offset2);
        let unshared_key = &self.block.data[offset3..offset3 + unshared_len as usize];
        self.key.truncate(shared_len as usize);
        self.key.extend_from_slice(unshared_key);
        let val_start = offset3 + unshared_len as usize;
        let val_end = val_start + value_len as usize;
        self.value = &self.block.data[val_start..val_end];

        self.offset = offset;
        self.next_offset = val_end;
    }

    pub fn seek_to_first(&mut self) {
        if self.block.restart_offset == 0 {
            return;
        }

        self.decode_entry_at(0);
    }

    pub fn next(&mut self) {
        if self.next_offset >= self.block.restart_offset {
            self.offset = self.next_offset;
            return;
        }

        self.decode_entry_at(self.next_offset);
    }

    pub fn seek(&mut self, target: &[u8]) {
        if self.block.restart_offset == 0 {
            return;
        }

        let mut left = 0;
        let mut right = self.block.num_restarts.saturating_sub(1);

        while left < right {
            let mid = (left + right).div_ceil(2);
            let bytes = &self.block.data
                [self.block.restart_offset + mid * 4..self.block.restart_offset + mid * 4 + 4];
            let entry_offset = u32::from_le_bytes(bytes.try_into().unwrap()) as usize;

            self.decode_entry_at(entry_offset);

            match self.key.as_slice().cmp(target) {
                Ordering::Equal => {
                    return;
                }
                Ordering::Less => {
                    left = mid;
                }
                Ordering::Greater => {
                    right = mid - 1;
                }
            }
        }

        let bytes = &self.block.data
            [self.block.restart_offset + left * 4..self.block.restart_offset + left * 4 + 4];
        let entry_offset = u32::from_le_bytes(bytes.try_into().unwrap()) as usize;
        self.decode_entry_at(entry_offset);

        while self.is_valid() && self.key.as_slice() < target {
            self.next();
        }
    }

    pub fn key(&self) -> Option<&[u8]> {
        if self.is_valid() {
            Some(&self.key)
        } else {
            None
        }
    }

    pub const fn value(&self) -> Option<&[u8]> {
        if self.is_valid() {
            Some(self.value)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_builder_and_iterator() {
        let mut builder = BlockBuilder::new(2);
        builder.add(b"apple", b"v1");
        builder.add(b"application", b"v2");
        builder.add(b"banana", b"v3");

        let raw_block = builder.finish().to_vec();
        let block = Block::new(raw_block);
        let mut iter = BlockIterator::new(&block);

        iter.seek_to_first();
        assert_eq!(iter.key(), Some(&b"apple"[..]));
        assert_eq!(iter.value(), Some(&b"v1"[..]));

        iter.next();
        assert_eq!(iter.key(), Some(&b"application"[..]));
        assert_eq!(iter.value(), Some(&b"v2"[..]));

        iter.next();
        assert_eq!(iter.key(), Some(&b"banana"[..]));
        assert_eq!(iter.value(), Some(&b"v3"[..]));

        iter.next();
        assert_eq!(iter.key(), None);
        assert_eq!(iter.value(), None);
        assert!(!iter.is_valid());

        iter.seek(b"application");
        assert_eq!(iter.key(), Some(&b"application"[..]));
        assert_eq!(iter.value(), Some(&b"v2"[..]));
        iter.seek(b"app");
        assert_eq!(iter.key(), Some(&b"apple"[..]));
        assert_eq!(iter.value(), Some(&b"v1"[..]));
    }
}
