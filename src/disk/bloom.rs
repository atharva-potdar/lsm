#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub struct BloomFilter {
    pub data: Vec<u8>,
    pub k: u8,
}

impl BloomFilter {
    pub fn new(num_keys: usize, bits_per_key: usize) -> Self {
        let total_bits = num_keys * bits_per_key;
        let mut total_bytes = total_bits / 8;
        if !total_bits.is_multiple_of(8) {
            total_bytes += 1;
        }

        // k = bits_per_key * ln(2)
        let k = (bits_per_key as f64 * 0.693) as u8;
        let k = k.clamp(1, 30);

        Self {
            data: vec![0; total_bytes.max(1)],
            k,
        }
    }

    pub fn add(&mut self, hash: u64) {
        let total_bits = self.data.len() * 8;

        let h1 = hash as u32;
        let h2 = (hash >> 32) as u32;

        for i in 0..self.k {
            let probe = h1.wrapping_add(u32::from(i).wrapping_mul(h2));
            let bit_index = (probe as usize) % total_bits;
            let byte_index = bit_index / 8;
            let bit_offset = bit_index % 8;
            self.data[byte_index] |= 1 << bit_offset;
        }
    }

    pub fn may_contain(&self, hash: u64) -> bool {
        let total_bits = self.data.len() * 8;
        let h1 = hash as u32;
        let h2 = (hash >> 32) as u32;

        for i in 0..self.k {
            let probe = h1.wrapping_add(u32::from(i).wrapping_mul(h2));
            let bit_index = (probe as usize) % total_bits;
            let byte_index = bit_index / 8;
            let bit_offset = bit_index % 8;

            if (self.data[byte_index] & (1 << bit_offset)) == 0 {
                return false;
            }
        }

        true
    }

    pub fn from_raw(data: Vec<u8>, k: u8) -> Self {
        Self {
            data,
            k
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_filter_basic() {
        let mut filter = BloomFilter::new(100, 10);

        filter.add(123456789);
        filter.add(987654321);

        assert!(filter.may_contain(123456789));
        assert!(filter.may_contain(987654321));

        assert!(!filter.may_contain(111111111));
    }
}
