#![allow(clippy::cast_possible_truncation)]

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub struct WalWriter {
    file: File,
}

pub struct WalReader {
    file: File,
}

impl WalWriter {
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().append(true).create(true).open(path)?;
        Ok(Self { file })
    }

    pub fn append(&mut self, internal_key: &[u8], value: &[u8]) -> std::io::Result<()> {
        let key_len = internal_key.len() as u32;
        self.file.write_all(&key_len.to_le_bytes())?;
        self.file.write_all(internal_key)?;

        let val_len = value.len() as u32;
        self.file.write_all(&val_len.to_le_bytes())?;
        self.file.write_all(value)?;

        self.file.flush()?;
        Ok(())
    }
}

impl WalReader {
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self { file })
    }

    pub fn next(&mut self) -> std::io::Result<Option<(Vec<u8>, Vec<u8>)>> {
        let mut len_buf = [0u8; 4];
        if let Err(e) = self.file.read_exact(&mut len_buf) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                return Ok(None);
            }
            return Err(e);
        }

        let key_len = u32::from_le_bytes(len_buf) as usize;
        let mut key = vec![0u8; key_len];
        self.file.read_exact(&mut key)?;

        let mut val_len_buf = [0u8; 4];
        self.file.read_exact(&mut val_len_buf)?;
        let value_len = u32::from_le_bytes(val_len_buf) as usize;
        let mut value = vec![0u8; value_len];
        self.file.read_exact(&mut value)?;

        Ok(Some((key, value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn test_wal_write_and_read() {
        let path = temp_dir().join("test_wal.log");

        // Ensure clean state
        let _ = std::fs::remove_file(&path);

        // 1. Write to the WAL
        let mut writer = WalWriter::new(&path).unwrap();
        writer.append(b"key1_internal", b"value1").unwrap();
        writer.append(b"key2_internal", b"value2").unwrap();

        // 2. Read from the WAL
        let mut reader = WalReader::new(&path).unwrap();

        let (k1, v1) = reader.next().unwrap().unwrap();
        assert_eq!(k1, b"key1_internal");
        assert_eq!(v1, b"value1");

        let (k2, v2) = reader.next().unwrap().unwrap();
        assert_eq!(k2, b"key2_internal");
        assert_eq!(v2, b"value2");

        // 3. Assert EOF
        assert!(reader.next().unwrap().is_none());

        let _ = std::fs::remove_file(&path);
    }
}
