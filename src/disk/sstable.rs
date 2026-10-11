pub struct BlockHandle {
    pub offset: u64,
    pub size: u64
}

pub struct Footer {
    pub filter_handle: BlockHandle,
    pub index_handle: BlockHandle,
    pub magic: u64,
}

impl BlockHandle {
    pub fn new(offset: u64, size: u64) -> BlockHandle {
        Self {
            offset,
            size
        }
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

        Self {
            offset,
            size
        }
    }
}

pub const SSTABLE_MAGIC: u64 = u64::from_le_bytes(*b"CATharva");

impl Footer {
    pub fn new(filter_handle: BlockHandle, index_handle: BlockHandle) -> Footer {
        Self {
            filter_handle,
            index_handle,
            magic: SSTABLE_MAGIC
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
            magic,
            filter_handle,
            index_handle
        })
    }
}