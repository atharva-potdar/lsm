#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use crate::disk::block::{Block, BlockBuilder, BlockIterator};
use crate::disk::bloom::BloomFilter;
use xxhash_rust::xxh3::xxh3_64;

use crate::memory::core::encode_internal_key;
use std::fs::File;
use std::os::unix::fs::FileExt;

pub struct BlockHandle {
    pub offset: u64,
    pub size: u64,
}

pub struct Footer {
    pub filter_handle: BlockHandle,
    pub index_handle: BlockHandle,
    pub magic: u64,
}

impl BlockHandle {
    pub const fn new(offset: u64, size: u64) -> Self {
        Self { offset, size }
    }

    pub fn encode(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];

        buf[0..8].copy_from_slice(&self.offset.to_le_bytes());
        buf[8..16].copy_from_slice(&self.size.to_le_bytes());

        buf
    }

    pub fn decode(bytes: &[u8; 16]) -> Self {
        let offset = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let size = u64::from_le_bytes(bytes[8..16].try_into().unwrap());

        Self { offset, size }
    }
}

pub const SSTABLE_MAGIC: u64 = u64::from_le_bytes(*b"CATharva");

impl Footer {
    pub const fn new(filter_handle: BlockHandle, index_handle: BlockHandle) -> Self {
        Self {
            filter_handle,
            index_handle,
            magic: SSTABLE_MAGIC,
        }
    }

    pub fn encode(&self) -> [u8; 40] {
        let mut buf = [0u8; 40];
        buf[0..16].copy_from_slice(&self.filter_handle.encode());
        buf[16..32].copy_from_slice(&self.index_handle.encode());
        buf[32..40].copy_from_slice(&self.magic.to_le_bytes());

        buf
    }

    pub fn decode(bytes: &[u8; 40]) -> Result<Self, &'static str> {
        let magic = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap());
        if magic != SSTABLE_MAGIC {
            return Err("invalid sstable magic number");
        }

        let filter_handle = BlockHandle::decode(bytes[0..16].try_into().unwrap());
        let index_handle = BlockHandle::decode(bytes[16..32].try_into().unwrap());

        Ok(Self {
            filter_handle,
            index_handle,
            magic,
        })
    }
}

pub struct SSTableWriter<W: std::io::Write> {
    writer: W,
    data_builder: BlockBuilder,
    index_builder: BlockBuilder,
    last_key: Vec<u8>,
    key_hashes: Vec<u64>,
    offset: u64,
    block_size: usize,
    data_restart_interval: usize,
    bits_per_key: usize,
}

impl<W: std::io::Write> SSTableWriter<W> {
    pub fn new(
        writer: W,
        block_size: usize,
        data_restart_interval: usize,
        index_restart_interval: usize,
        bits_per_key: usize,
    ) -> Self {
        Self {
            writer,
            data_builder: BlockBuilder::new(data_restart_interval),
            index_builder: BlockBuilder::new(index_restart_interval),
            last_key: Vec::new(),
            key_hashes: Vec::new(),
            offset: 0,
            block_size,
            data_restart_interval,
            bits_per_key,
        }
    }

    fn flush_data_block(&mut self) -> std::io::Result<()> {
        if self.data_builder.is_empty() {
            return Ok(());
        }

        let block_bytes = self.data_builder.finish();
        self.writer.write_all(block_bytes)?;

        let handle = BlockHandle::new(self.offset, block_bytes.len() as u64);
        self.index_builder.add(&self.last_key, &handle.encode());

        self.offset += block_bytes.len() as u64;
        self.data_builder = BlockBuilder::new(self.data_restart_interval);
        Ok(())
    }

    pub fn add(&mut self, key: &[u8], value: &[u8]) -> std::io::Result<()> {
        assert!(key.len() >= 9);

        if self.data_builder.estimated_size() >= self.block_size {
            self.flush_data_block()?;
        }

        self.data_builder.add(key, value);
        self.last_key.clear();
        self.last_key.extend_from_slice(key);
        self.key_hashes.push(xxh3_64(&key[..key.len() - 9]));
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<W> {
        self.flush_data_block()?;

        let mut filter = BloomFilter::new(self.key_hashes.len(), self.bits_per_key);
        for hash in &self.key_hashes {
            filter.add(*hash);
        }
        self.writer.write_all(&filter.data)?;
        self.writer.write_all(&[filter.k])?;
        let filter_handle = BlockHandle::new(self.offset, (filter.data.len() + 1) as u64);
        self.offset += (filter.data.len() + 1) as u64;

        let index_bytes = self.index_builder.finish();
        self.writer.write_all(index_bytes)?;
        let index_handle = BlockHandle::new(self.offset, index_bytes.len() as u64);
        self.offset += index_bytes.len() as u64;

        let footer = Footer::new(filter_handle, index_handle);
        self.writer.write_all(&footer.encode())?;

        self.writer.flush()?;
        Ok(self.writer)
    }
}

pub struct SsTable {
    file: File,
    index_block: Block,
    filter: BloomFilter,
}

impl SsTable {
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let file_len = file.metadata()?.len();
        if file_len < 40 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let mut footer_buf = [0u8; 40];
        file.read_exact_at(&mut footer_buf, file_len - 40)?;
        let footer = Footer::decode(&footer_buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut filter_block: Vec<u8> = vec![0u8; footer.filter_handle.size as usize];
        file.read_exact_at(&mut filter_block, footer.filter_handle.offset)?;
        let k = filter_block.pop().unwrap();
        let filter = BloomFilter::from_raw(filter_block, k);
        let mut i_block: Vec<u8> = vec![0u8; footer.index_handle.size as usize];
        file.read_exact_at(&mut i_block, footer.index_handle.offset)?;
        let index_block = Block::decode(i_block)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Self {
            file,
            index_block,
            filter,
        })
    }

    pub fn get(&self, user_key: &[u8], read_lsn: u64) -> std::io::Result<Option<Vec<u8>>> {
        let hash = xxh3_64(user_key);
        if !self.filter.may_contain(hash) {
            return Ok(None);
        }

        let probe = encode_internal_key(user_key, read_lsn, 1);
        let mut iterator = BlockIterator::new(&self.index_block);
        iterator.seek(probe.as_slice());
        if !iterator.is_valid() {
            return Ok(None);
        }
        let handle = BlockHandle::decode(iterator.value().unwrap()[0..16].try_into().unwrap());
        let mut buf = vec![0u8; handle.size as usize];
        self.file.read_exact_at(&mut buf, handle.offset)?;
        let block = Block::decode(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut iterator = BlockIterator::new(&block);
        iterator.seek(probe.as_slice());
        if !iterator.is_valid() {
            return Ok(None);
        }
        let internal_key = iterator.key().unwrap();
        if internal_key.len() < 9 {
            return Ok(None);
        }
        let key = &internal_key[..internal_key.len() - 9];
        if key != user_key {
            return Ok(None);
        }
        let op_type = *internal_key.last().unwrap();
        if op_type == 1 {
            Ok(Some(iterator.value().unwrap().to_vec()))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::core::encode_internal_key;

    #[test]
    fn test_sstable_build_basic() {
        let buf = Vec::new();
        let mut writer = SSTableWriter::new(buf, 4096, 16, 1, 10);
        let k1 = encode_internal_key(b"user1", 100, 1);
        let k2 = encode_internal_key(b"user2", 90, 1);

        writer.add(&k1, b"val1").unwrap();
        writer.add(&k2, b"val2").unwrap();

        let sst_bytes = writer.finish().unwrap();
        assert!(sst_bytes.len() > 40);
        let footer_slice: &[u8; 40] = sst_bytes[sst_bytes.len() - 40..].try_into().unwrap();
        let footer = Footer::decode(footer_slice).unwrap();
        assert_eq!(footer.magic, SSTABLE_MAGIC);
        assert!(footer.index_handle.size > 0);
        assert!(footer.filter_handle.size > 0);
    }

    #[test]
    fn test_sstable_read_basic() {
        use std::env::temp_dir;
        let path = temp_dir().join("test_sstable_get.sst");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = SSTableWriter::new(file, 4096, 16, 1, 10);
        writer
            .add(encode_internal_key(b"apple", 100, 1).as_slice(), b"red")
            .unwrap();
        writer
            .add(encode_internal_key(b"banana", 105, 0).as_slice(), b"")
            .unwrap();
        writer
            .add(encode_internal_key(b"banana", 100, 1).as_slice(), b"yellow")
            .unwrap();
        writer
            .add(encode_internal_key(b"zebra", 110, 1).as_slice(), b"stripes")
            .unwrap();
        writer.finish().unwrap();

        let table = SsTable::open(&path).unwrap();
        assert_eq!(table.get(b"apple", 110).unwrap(), Some(b"red".to_vec()));
        assert_eq!(table.get(b"banana", 100).unwrap(), Some(b"yellow".to_vec()));
        assert_eq!(table.get(b"banana", 110).unwrap(), None);
        assert_eq!(table.get(b"ghost", 110).unwrap(), None);
    }
}
