#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
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
}
