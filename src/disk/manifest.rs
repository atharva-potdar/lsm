#[allow(unused_imports)]
use std::collections::HashSet;

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

#[derive(Clone, Debug)]
pub struct VersionSet {
    pub current: Version,
    pub next_file_id: u64,
    // In a real engine, you'd hold the `manifest` File writer here.
}

impl VersionEdit {
    pub fn add_file(&mut self, level: usize, meta: FileMetaData) {
        self.new_files.push((level, meta));
    }

    pub fn delete_file(&mut self, level: usize, file_id: u64) {
        self.deleted_files.push((level, file_id));
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
            level_files.sort_by(|a, b| a.min_key.cmp(&b.min_key));
        }

        Self { files: next_files }
    }
}

impl VersionSet {
    #[must_use]
    pub fn new(num_levels: usize) -> Self {
        Self {
            current: Version::new(num_levels),
            next_file_id: 1,
        }
    }

    pub const fn new_file_id(&mut self) -> u64 {
        let id = self.next_file_id;
        self.next_file_id += 1;
        id
    }

    pub fn log_and_apply(&mut self, edit: &VersionEdit) {
        let next_version = self.current.apply(edit);
        self.current = next_version;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        vset.log_and_apply(&edit);

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
        vset.log_and_apply(&edit2);

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
        vset.log_and_apply(&edit);

        // Level 0 must maintain insertion order (not sorted)
        assert_eq!(vset.current.files[0].len(), 2);
        assert_eq!(vset.current.files[0][0].file_id, 1);
        assert_eq!(vset.current.files[0][1].file_id, 2);
    }
}
