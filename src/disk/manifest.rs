#![allow(
    clippy::cast_possible_truncation,
    clippy::unnecessary_wraps
)]

use crate::memory::core::cmp_internal_key;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMetaData {
    pub file_id: u64,
    pub file_size: u64,
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionEdit {
    pub deleted_files: Vec<(usize, u64)>, // (level, file_id)
    pub new_files: Vec<(usize, FileMetaData)>, // (level, meta)
}

#[derive(Clone, Debug)]
pub struct Version {
    pub files: Vec<Vec<FileMetaData>>, // Index is the level (0 to 6)
}

pub struct VersionSet {
    pub work_dir: PathBuf,
    pub current: Arc<Version>,
    pub versions: Vec<Weak<Version>>,
    pub obsolete_files: HashSet<u64>,
    pub next_file_id: u64,
    pub last_sequence: u64,
    pub manifest_file: Option<File>,
}

impl VersionEdit {
    pub fn add_file(&mut self, level: usize, meta: FileMetaData) {
        self.new_files.push((level, meta));
    }

    pub fn delete_file(&mut self, level: usize, file_id: u64) {
        self.deleted_files.push((level, file_id));
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        buf.extend_from_slice(&(self.deleted_files.len() as u32).to_le_bytes());
        for &(level, file_id) in &self.deleted_files {
            buf.extend_from_slice(&(level as u32).to_le_bytes());
            buf.extend_from_slice(&file_id.to_le_bytes());
        }

        buf.extend_from_slice(&(self.new_files.len() as u32).to_le_bytes());
        for (level, meta) in &self.new_files {
            buf.extend_from_slice(&(*level as u32).to_le_bytes());
            buf.extend_from_slice(&meta.file_id.to_le_bytes());
            buf.extend_from_slice(&meta.file_size.to_le_bytes());
            buf.extend_from_slice(&(meta.min_key.len() as u32).to_le_bytes());
            buf.extend_from_slice(&meta.min_key);
            buf.extend_from_slice(&(meta.max_key.len() as u32).to_le_bytes());
            buf.extend_from_slice(&meta.max_key);
        }

        buf
    }

    pub fn decode(data: &[u8]) -> Result<Self, &'static str> {
        let mut offset = 0;

        if data.len() < 4 {
            return Err("data too short for deleted_files count");
        }
        let num_deleted = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        let mut deleted_files = Vec::with_capacity(num_deleted);
        for _ in 0..num_deleted {
            if offset + 12 > data.len() {
                return Err("unexpected EOF reading deleted_files");
            }
            let level = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            let file_id = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            deleted_files.push((level, file_id));
        }

        if offset + 4 > data.len() {
            return Err("unexpected EOF reading new_files count");
        }
        let num_new = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        let mut new_files = Vec::with_capacity(num_new);
        for _ in 0..num_new {
            if offset + 20 > data.len() {
                return Err("unexpected EOF reading new_files header");
            }
            let level = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            let file_id = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let file_size = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;

            if offset + 4 > data.len() {
                return Err("unexpected EOF reading min_key len");
            }
            let min_key_len =
                u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            if offset + min_key_len > data.len() {
                return Err("unexpected EOF reading min_key");
            }
            let min_key = data[offset..offset + min_key_len].to_vec();
            offset += min_key_len;

            if offset + 4 > data.len() {
                return Err("unexpected EOF reading max_key len");
            }
            let max_key_len =
                u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            if offset + max_key_len > data.len() {
                return Err("unexpected EOF reading max_key");
            }
            let max_key = data[offset..offset + max_key_len].to_vec();
            offset += max_key_len;

            new_files.push((
                level,
                FileMetaData {
                    file_id,
                    file_size,
                    min_key,
                    max_key,
                },
            ));
        }

        Ok(Self {
            deleted_files,
            new_files,
        })
    }
}

impl Version {
    #[must_use]
    pub fn new(num_levels: usize) -> Self {
        Self {
            files: vec![Vec::new(); num_levels],
        }
    }

    #[must_use]
    pub fn apply(&self, edit: &VersionEdit) -> Self {
        let mut next_files = self.files.clone();

        for &(level, file_id) in &edit.deleted_files {
            if level < next_files.len() {
                next_files[level].retain(|f| f.file_id != file_id);
            }
        }

        for (level, meta) in &edit.new_files {
            if *level < next_files.len() {
                next_files[*level].push(meta.clone());
            }
        }

        for level_files in next_files.iter_mut().skip(1) {
            level_files.sort_by(|a, b| cmp_internal_key(&a.min_key, &b.min_key));
        }

        Self { files: next_files }
    }
}

impl VersionSet {
    #[must_use]
    pub fn new(num_levels: usize) -> Self {
        Self {
            work_dir: PathBuf::new(),
            current: Arc::new(Version::new(num_levels)),
            versions: Vec::new(),
            obsolete_files: HashSet::new(),
            next_file_id: 1,
            last_sequence: 0,
            manifest_file: None,
        }
    }

    pub fn open(work_dir: impl AsRef<Path>, num_levels: usize) -> std::io::Result<Self> {
        let work_dir = work_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&work_dir)?;
        let manifest_path = work_dir.join("manifest.log");

        let mut current = Version::new(num_levels);
        let mut max_file_id = 0;
        let mut max_lsn = 0;
        let mut obsolete_files = HashSet::new();

        if manifest_path.exists() {
            let mut file = File::open(&manifest_path)?;
            let mut len_buf = [0u8; 4];
            loop {
                match file.read_exact(&mut len_buf) {
                    Ok(()) => {
                        let len = u32::from_le_bytes(len_buf) as usize;
                        let mut edit_buf = vec![0u8; len];
                        if let Err(e) = file.read_exact(&mut edit_buf) {
                            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                                break;
                            }
                            return Err(e);
                        }
                        let edit = VersionEdit::decode(&edit_buf).map_err(|e| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
                        })?;

                        for &(_, file_id) in &edit.deleted_files {
                            max_file_id = max_file_id.max(file_id);
                            obsolete_files.insert(file_id);
                        }
                        for (_, meta) in &edit.new_files {
                            max_file_id = max_file_id.max(meta.file_id);
                            if meta.max_key.len() >= 9 {
                                let lsn_bytes: [u8; 8] = meta.max_key
                                    [meta.max_key.len() - 9..meta.max_key.len() - 1]
                                    .try_into()
                                    .unwrap();
                                let lsn = u64::MAX - u64::from_be_bytes(lsn_bytes);
                                max_lsn = max_lsn.max(lsn);
                            }
                            if meta.min_key.len() >= 9 {
                                let lsn_bytes: [u8; 8] = meta.min_key
                                    [meta.min_key.len() - 9..meta.min_key.len() - 1]
                                    .try_into()
                                    .unwrap();
                                let lsn = u64::MAX - u64::from_be_bytes(lsn_bytes);
                                max_lsn = max_lsn.max(lsn);
                            }
                        }

                        current = current.apply(&edit);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        let manifest_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&manifest_path)?;

        let next_file_id = if max_file_id == 0 { 1 } else { max_file_id + 1 };

        let mut vset = Self {
            work_dir,
            current: Arc::new(current),
            versions: Vec::new(),
            obsolete_files,
            next_file_id,
            last_sequence: max_lsn,
            manifest_file: Some(manifest_file),
        };

        let _ = vset.clean_obsolete_files();

        Ok(vset)
    }

    #[must_use]
    pub fn current(&self) -> Arc<Version> {
        Arc::clone(&self.current)
    }

    pub const fn new_file_id(&mut self) -> u64 {
        let id = self.next_file_id;
        self.next_file_id += 1;
        id
    }

    pub fn log_and_apply(&mut self, edit: &VersionEdit) -> std::io::Result<()> {
        if let Some(file) = &mut self.manifest_file {
            let encoded = edit.encode();
            let len = encoded.len() as u32;
            file.write_all(&len.to_le_bytes())?;
            file.write_all(&encoded)?;
            file.sync_data()?;
        }

        for &(_, file_id) in &edit.deleted_files {
            self.obsolete_files.insert(file_id);
        }

        for (_, meta) in &edit.new_files {
            self.next_file_id = self.next_file_id.max(meta.file_id + 1);
            if meta.max_key.len() >= 9 {
                let lsn_bytes: [u8; 8] = meta.max_key
                    [meta.max_key.len() - 9..meta.max_key.len() - 1]
                    .try_into()
                    .unwrap();
                let lsn = u64::MAX - u64::from_be_bytes(lsn_bytes);
                self.last_sequence = self.last_sequence.max(lsn);
            }
            if meta.min_key.len() >= 9 {
                let lsn_bytes: [u8; 8] = meta.min_key
                    [meta.min_key.len() - 9..meta.min_key.len() - 1]
                    .try_into()
                    .unwrap();
                let lsn = u64::MAX - u64::from_be_bytes(lsn_bytes);
                self.last_sequence = self.last_sequence.max(lsn);
            }
        }

        let next_version = Arc::new(self.current.apply(edit));
        self.versions.retain(|w| w.strong_count() > 0);
        self.versions.push(Arc::downgrade(&self.current));
        self.current = next_version;

        let _ = self.clean_obsolete_files();

        Ok(())
    }

    pub fn clean_obsolete_files(&mut self) -> std::io::Result<Vec<u64>> {
        self.versions.retain(|w| w.strong_count() > 0);

        let mut live_files = HashSet::new();
        for level_files in &self.current.files {
            for f in level_files {
                live_files.insert(f.file_id);
            }
        }
        for weak in &self.versions {
            if let Some(v) = weak.upgrade() {
                for level_files in &v.files {
                    for f in level_files {
                        live_files.insert(f.file_id);
                    }
                }
            }
        }

        let mut removed = Vec::new();
        for &file_id in &self.obsolete_files {
            if !live_files.contains(&file_id) {
                if !self.work_dir.as_os_str().is_empty() {
                    let path = self.work_dir.join(format!("{file_id:06}.sst"));
                    let _ = std::fs::remove_file(&path);
                }
                removed.push(file_id);
            }
        }

        for file_id in &removed {
            self.obsolete_files.remove(file_id);
        }

        Ok(removed)
    }
}

impl Drop for VersionSet {
    fn drop(&mut self) {
        let _ = self.clean_obsolete_files();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_edit_encode_decode() {
        let mut edit = VersionEdit::default();
        edit.delete_file(0, 10);
        edit.delete_file(1, 15);

        let meta = FileMetaData {
            file_id: 20,
            file_size: 4096,
            min_key: b"apple".to_vec(),
            max_key: b"banana".to_vec(),
        };
        edit.add_file(1, meta);

        let encoded = edit.encode();
        let decoded = VersionEdit::decode(&encoded).unwrap();
        assert_eq!(edit, decoded);
    }

    #[test]
    fn test_manifest_persistence_and_recovery() {
        let temp_dir = std::env::temp_dir().join("lsm_test_manifest_recovery");
        let _ = std::fs::remove_dir_all(&temp_dir);

        {
            let mut vset = VersionSet::open(&temp_dir, 7).unwrap();
            let mut edit = VersionEdit::default();
            edit.add_file(
                0,
                FileMetaData {
                    file_id: 1,
                    file_size: 1024,
                    min_key: b"k1".to_vec(),
                    max_key: b"k2".to_vec(),
                },
            );
            edit.add_file(
                0,
                FileMetaData {
                    file_id: 2,
                    file_size: 2048,
                    min_key: b"k3".to_vec(),
                    max_key: b"k4".to_vec(),
                },
            );
            vset.log_and_apply(&edit).unwrap();
            assert_eq!(vset.next_file_id, 3);
        }

        // Reopen from disk
        {
            let vset = VersionSet::open(&temp_dir, 7).unwrap();
            assert_eq!(vset.current.files[0].len(), 2);
            assert_eq!(vset.current.files[0][0].file_id, 1);
            assert_eq!(vset.current.files[0][1].file_id, 2);
            // next_file_id must be MAX(file_id) + 1 = 2 + 1 = 3
            assert_eq!(vset.next_file_id, 3);
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_version_set_new_file_id() {
        let mut vset = VersionSet::new(7);
        assert_eq!(vset.new_file_id(), 1);
        assert_eq!(vset.new_file_id(), 2);
        assert_eq!(vset.new_file_id(), 3);
        assert_eq!(vset.next_file_id, 4);
    }

    #[test]
    fn test_version_apply_add_and_delete() {
        let mut vset = VersionSet::new(7);
        let mut edit = VersionEdit::default();

        let meta1 = FileMetaData {
            file_id: 1,
            file_size: 1024,
            min_key: b"b".to_vec(),
            max_key: b"d".to_vec(),
        };
        let meta2 = FileMetaData {
            file_id: 2,
            file_size: 2048,
            min_key: b"a".to_vec(),
            max_key: b"c".to_vec(),
        };

        // Add both to level 1
        edit.add_file(1, meta1);
        edit.add_file(1, meta2);
        vset.log_and_apply(&edit).unwrap();

        // Level 1 should be sorted by min_key: meta2 ("a") then meta1 ("b")
        assert_eq!(vset.current.files[1].len(), 2);
        assert_eq!(vset.current.files[1][0].file_id, 2);
        assert_eq!(vset.current.files[1][1].file_id, 1);

        // Delete file 2 from level 1 and add file 3 to level 0
        let mut edit2 = VersionEdit::default();
        edit2.delete_file(1, 2);
        let meta3 = FileMetaData {
            file_id: 3,
            file_size: 512,
            min_key: b"z".to_vec(),
            max_key: b"z".to_vec(),
        };
        edit2.add_file(0, meta3);
        vset.log_and_apply(&edit2).unwrap();

        assert_eq!(vset.current.files[1].len(), 1);
        assert_eq!(vset.current.files[1][0].file_id, 1);
        assert_eq!(vset.current.files[0].len(), 1);
        assert_eq!(vset.current.files[0][0].file_id, 3);
    }

    #[test]
    fn test_level_0_not_sorted() {
        let mut vset = VersionSet::new(7);
        let mut edit = VersionEdit::default();

        let meta1 = FileMetaData {
            file_id: 1,
            file_size: 1024,
            min_key: b"z".to_vec(),
            max_key: b"z".to_vec(),
        };
        let meta2 = FileMetaData {
            file_id: 2,
            file_size: 2048,
            min_key: b"a".to_vec(),
            max_key: b"a".to_vec(),
        };

        edit.add_file(0, meta1);
        edit.add_file(0, meta2);
        vset.log_and_apply(&edit).unwrap();

        // Level 0 must maintain insertion order (not sorted)
        assert_eq!(vset.current.files[0].len(), 2);
        assert_eq!(vset.current.files[0][0].file_id, 1);
        assert_eq!(vset.current.files[0][1].file_id, 2);
    }
}
