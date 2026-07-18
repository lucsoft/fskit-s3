//! An in-memory [`StorageBackend`] over a flat key→bytes map.
//!
//! Deliberately models the same shape as an object store: there are no real
//! directories, only keys, and a "directory" is any prefix that keys share. That
//! makes it a faithful stand-in for S3 in tests and a zero-setup mount target
//! for bringing the FSKit side up before real credentials exist.

use std::collections::BTreeMap;

use crate::{async_trait, path, Entry, StorageBackend, StorageError};

/// A read-only in-memory store. Keys are object paths without a leading slash,
/// e.g. `photos/2026/img.jpg`.
#[derive(Debug, Default, Clone)]
pub struct InMemoryBackend {
    objects: BTreeMap<String, Vec<u8>>,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an object at `key` (a slash-separated path, no leading slash).
    pub fn insert(&mut self, key: impl Into<String>, bytes: impl Into<Vec<u8>>) -> &mut Self {
        self.objects.insert(key.into(), bytes.into());
        self
    }

    /// Does any key sit under this prefix? (Used to decide a path is a
    /// directory even though no object is stored at it.)
    fn is_prefix(&self, prefix: &str) -> bool {
        self.objects.keys().any(|k| k.starts_with(prefix))
    }
}

#[async_trait]
impl StorageBackend for InMemoryBackend {
    async fn list(&self, dir: &str) -> Result<Vec<Entry>, StorageError> {
        let dir = path::normalize(dir);
        let prefix = path::to_key(&dir, true); // "" for root, "a/b/" otherwise

        // The root always lists; a non-root dir must actually be a prefix.
        if !prefix.is_empty() && !self.is_prefix(&prefix) {
            // Distinguish "it's a file" from "doesn't exist".
            let file_key = path::to_key(&dir, false);
            return if self.objects.contains_key(&file_key) {
                Err(StorageError::NotADirectory)
            } else {
                Err(StorageError::NotFound)
            };
        }

        // Collapse the flat key space into immediate children, S3-style: the
        // first path component after `prefix` is either a file (no further `/`)
        // or a synthesized subdirectory (has one).
        let mut files: Vec<Entry> = Vec::new();
        let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (key, bytes) in &self.objects {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            match rest.split_once('/') {
                Some((child, _)) => {
                    dirs.insert(child.to_string());
                }
                None => files.push(Entry::file(rest.to_string(), bytes.len() as u64)),
            }
        }

        let mut out: Vec<Entry> = dirs.into_iter().map(Entry::dir).collect();
        out.extend(files);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<Entry, StorageError> {
        let norm = path::normalize(path);
        if norm == "/" {
            return Ok(Entry::dir(""));
        }
        let name = path::basename(&norm).to_string();
        let file_key = path::to_key(&norm, false);
        if let Some(bytes) = self.objects.get(&file_key) {
            return Ok(Entry::file(name, bytes.len() as u64));
        }
        let dir_prefix = path::to_key(&norm, true);
        if self.is_prefix(&dir_prefix) {
            return Ok(Entry::dir(name));
        }
        Err(StorageError::NotFound)
    }

    async fn read(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, StorageError> {
        let norm = path::normalize(path);
        let key = path::to_key(&norm, false);
        let Some(bytes) = self.objects.get(&key) else {
            // A prefix with no object at the exact key is a directory.
            return if self.is_prefix(&path::to_key(&norm, true)) {
                Err(StorageError::NotAFile)
            } else {
                Err(StorageError::NotFound)
            };
        };
        let start = (offset as usize).min(bytes.len());
        let end = start.saturating_add(len).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntryKind;

    fn sample() -> InMemoryBackend {
        let mut b = InMemoryBackend::new();
        b.insert("readme.txt", b"hello world".to_vec())
            .insert("photos/2026/a.jpg", vec![0u8; 10])
            .insert("photos/2026/b.jpg", vec![0u8; 20])
            .insert("photos/cover.png", vec![0u8; 5]);
        b
    }

    #[tokio::test]
    async fn root_lists_files_and_synth_dirs() {
        let b = sample();
        let entries = b.list("/").await.unwrap();
        let names: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(
            names,
            vec![("photos", EntryKind::Dir), ("readme.txt", EntryKind::File)]
        );
    }

    #[tokio::test]
    async fn nested_listing() {
        let b = sample();
        let entries = b.list("/photos").await.unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["2026", "cover.png"]);

        let deep = b.list("/photos/2026").await.unwrap();
        assert_eq!(deep.len(), 2);
        assert_eq!(deep[0].name, "a.jpg");
        assert_eq!(deep[0].size, 10);
    }

    #[tokio::test]
    async fn stat_variants() {
        let b = sample();
        assert!(b.stat("/").await.unwrap().is_dir());
        assert!(b.stat("/photos").await.unwrap().is_dir());
        assert_eq!(b.stat("/readme.txt").await.unwrap().size, 11);
        assert!(matches!(b.stat("/nope").await, Err(StorageError::NotFound)));
    }

    #[tokio::test]
    async fn ranged_reads_and_eof() {
        let b = sample();
        assert_eq!(b.read("/readme.txt", 0, 5).await.unwrap(), b"hello");
        assert_eq!(b.read("/readme.txt", 6, 100).await.unwrap(), b"world"); // short read at EOF
        assert_eq!(b.read("/readme.txt", 100, 10).await.unwrap(), b""); // past EOF
        assert!(matches!(
            b.read("/photos", 0, 1).await,
            Err(StorageError::NotAFile)
        ));
        assert!(matches!(
            b.read("/missing", 0, 1).await,
            Err(StorageError::NotFound)
        ));
    }

    #[tokio::test]
    async fn listing_a_file_is_notdir() {
        let b = sample();
        assert!(matches!(
            b.list("/readme.txt").await,
            Err(StorageError::NotADirectory)
        ));
    }
}
