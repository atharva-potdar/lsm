#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use crate::disk::block::{Block, BlockBuilder, BlockIterator};
use crate::disk::bloom::BloomFilter;
use xxhash_rust::xxh3::xxh3_64;

use crate::memory::core::{ValueResult, encode_internal_key};
use crate::metrics::EngineMetrics;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::Ordering;

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

    pub const fn estimated_size(&self) -> usize {
        (self.offset as usize) + self.data_builder.estimated_size()
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
    pub(crate) file: File,
    pub(crate) index_block: Block,
    pub(crate) filter: BloomFilter,
}

use crate::disk::merge::StorageIterator;
use std::sync::Arc;

pub struct SsTableIterator {
    table: Arc<SsTable>,
    block_handles: Vec<BlockHandle>,
    current_block_idx: usize,
    current_block: Option<Block>,
    offset: usize,
    next_offset: usize,
    key: Vec<u8>,
    value: Vec<u8>,
    valid: bool,
}

impl SsTableIterator {
    pub fn new(table: Arc<SsTable>) -> std::io::Result<Self> {
        let mut index_iter = BlockIterator::new(&table.index_block);
        index_iter.seek_to_first();
        let mut block_handles = Vec::new();
        while index_iter.is_valid() {
            let handle_bytes: [u8; 16] = index_iter.value().unwrap()[0..16].try_into().unwrap();
            block_handles.push(BlockHandle::decode(&handle_bytes));
            index_iter.next();
        }

        let mut iter = Self {
            table,
            block_handles,
            current_block_idx: 0,
            current_block: None,
            offset: 0,
            next_offset: 0,
            key: Vec::new(),
            value: Vec::new(),
            valid: false,
        };

        if !iter.block_handles.is_empty() {
            iter.load_block(0)?;
        }

        Ok(iter)
    }

    fn load_block(&mut self, idx: usize) -> std::io::Result<bool> {
        if idx >= self.block_handles.len() {
            self.valid = false;
            self.current_block = None;
            return Ok(false);
        }
        let handle = &self.block_handles[idx];
        let mut buf = vec![0u8; handle.size as usize];
        self.table.file.read_exact_at(&mut buf, handle.offset)?;
        let block = Block::decode(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        self.current_block_idx = idx;
        self.offset = 0;
        self.next_offset = 0;

        if let Some((next_offset, val_start, val_len)) = block.decode_entry(0, &mut self.key) {
            self.value.clear();
            self.value
                .extend_from_slice(&block.data()[val_start..val_start + val_len]);
            self.offset = 0;
            self.next_offset = next_offset;
            self.valid = true;
            self.current_block = Some(block);
            return Ok(true);
        }

        self.current_block = Some(block);
        self.valid = false;
        Ok(false)
    }
}

impl StorageIterator for SsTableIterator {
    fn is_valid(&self) -> bool {
        self.valid
    }

    fn key(&self) -> &[u8] {
        &self.key
    }

    fn value(&self) -> &[u8] {
        &self.value
    }

    #[allow(clippy::collapsible_if)]
    fn next(&mut self) {
        if !self.valid {
            return;
        }

        if let Some(ref block) = self.current_block {
            if self.next_offset < block.restart_offset() {
                if let Some((next_offset, val_start, val_len)) =
                    block.decode_entry(self.next_offset, &mut self.key)
                {
                    self.value.clear();
                    self.value
                        .extend_from_slice(&block.data()[val_start..val_start + val_len]);
                    self.offset = self.next_offset;
                    self.next_offset = next_offset;
                    return;
                }
            }
        }

        let next_idx = self.current_block_idx + 1;
        let _ = self.load_block(next_idx);
    }
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

    pub fn iter(self: &Arc<Self>) -> std::io::Result<SsTableIterator> {
        SsTableIterator::new(Arc::clone(self))
    }

    pub fn get(&self, user_key: &[u8], read_lsn: u64) -> std::io::Result<ValueResult> {
        self.get_with_metrics(user_key, read_lsn, None)
    }

    pub fn get_with_metrics(
        &self,
        user_key: &[u8],
        read_lsn: u64,
        metrics: Option<&EngineMetrics>,
    ) -> std::io::Result<ValueResult> {
        let hash = xxh3_64(user_key);
        if !self.filter.may_contain(hash) {
            return Ok(ValueResult::NotFound);
        }

        let probe = encode_internal_key(user_key, read_lsn, 0);
        let mut iterator = BlockIterator::new(&self.index_block);
        iterator.seek(probe.as_slice());
        if !iterator.is_valid() {
            return Ok(ValueResult::NotFound);
        }
        let handle = BlockHandle::decode(iterator.value().unwrap()[0..16].try_into().unwrap());
        let mut buf = vec![0u8; handle.size as usize];
        self.file.read_exact_at(&mut buf, handle.offset)?;
        if let Some(m) = metrics {
            m.bytes_read_disk.fetch_add(handle.size, Ordering::Relaxed);
        }
        let block = Block::decode(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut iterator = BlockIterator::new(&block);
        iterator.seek(probe.as_slice());
        if !iterator.is_valid() {
            return Ok(ValueResult::NotFound);
        }
        let internal_key = iterator.key().unwrap();
        if internal_key.len() < 9 {
            return Ok(ValueResult::NotFound);
        }
        let key = &internal_key[..internal_key.len() - 9];
        if key != user_key {
            return Ok(ValueResult::NotFound);
        }
        let op_type = *internal_key.last().unwrap();
        if op_type == 1 {
            Ok(ValueResult::Found(iterator.value().unwrap().to_vec()))
        } else {
            Ok(ValueResult::Deleted)
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
        assert_eq!(
            table.get(b"apple", 110).unwrap(),
            ValueResult::Found(b"red".to_vec())
        );
        assert_eq!(
            table.get(b"banana", 100).unwrap(),
            ValueResult::Found(b"yellow".to_vec())
        );
        assert_eq!(table.get(b"banana", 110).unwrap(), ValueResult::Deleted);
        assert_eq!(table.get(b"ghost", 110).unwrap(), ValueResult::NotFound);
    }

    #[test]
    fn test_sstable_iterator() {
        use std::env::temp_dir;
        let path = temp_dir().join("test_sstable_iter.sst");
        let file = std::fs::File::create(&path).unwrap();

        // Use a small block size (64 bytes) to force creation of multiple data blocks
        let mut writer = SSTableWriter::new(file, 64, 4, 1, 10);
        let mut expected = Vec::new();

        for i in 0..30 {
            let key_str = format!("key_{i:04}");
            let val_str = format!("val_{i:04}");
            let k = encode_internal_key(key_str.as_bytes(), 100, 1);
            writer.add(&k, val_str.as_bytes()).unwrap();
            expected.push((k, val_str.into_bytes()));
        }
        writer.finish().unwrap();

        let table = Arc::new(SsTable::open(&path).unwrap());
        let mut iter = table.iter().unwrap();

        let mut collected = Vec::new();
        while iter.is_valid() {
            collected.push((iter.key().to_vec(), iter.value().to_vec()));
            iter.next();
        }

        assert_eq!(collected, expected);
        let _ = std::fs::remove_file(&path);
    }
}
