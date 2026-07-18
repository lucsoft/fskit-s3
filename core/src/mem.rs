//! An in-memory [`StorageBackend`] over a flat key→bytes map.
//!
//! Deliberately models the same shape as an object store: there are no real
//! directories, only keys, and a "directory" is any prefix that keys share (an
//! explicit empty directory is a zero-byte marker key ending in `/`). That makes
//! it a faithful stand-in for S3 in tests and a zero-setup, read-write mount
//! target for bringing the FSKit side up before real credentials exist.

use std::collections::BTreeMap;
use std::sync::RwLock;

use crate::{async_trait, path, Entry, EntryKind, StorageBackend, StorageError};

/// An in-memory store. Keys are object paths without a leading slash, e.g.
/// `photos/2026/img.jpg`. Mutating operations take `&self` (the trait is shared
/// behind an `Arc`), so the map lives behind an [`RwLock`].
#[derive(Debug, Default)]
pub struct InMemoryBackend {
    objects: RwLock<BTreeMap<String, Vec<u8>>>,
}

/// Any key under this prefix means the prefix is a directory.
fn is_prefix(objects: &BTreeMap<String, Vec<u8>>, prefix: &str) -> bool {
    objects.keys().any(|k| k.starts_with(prefix))
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an object at `key` (a slash-separated path, no leading slash).
    /// Builder-style seeding for tests and the demo mount.
    pub fn insert(&mut self, key: impl Into<String>, bytes: impl Into<Vec<u8>>) -> &mut Self {
        // `&mut self` means no other reference exists, so `get_mut` never blocks;
        // it still returns a `Result` for lock poisoning, which we ignore here.
        if let Ok(objects) = self.objects.get_mut() {
            objects.insert(key.into(), bytes.into());
        }
        self
    }

    /// Shared read access to the map, mapping a poisoned lock to a backend error
    /// (keeps the code panic-free — no `unwrap` on the lock).
    fn read_objects(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, BTreeMap<String, Vec<u8>>>, StorageError> {
        self.objects
            .read()
            .map_err(|_| StorageError::Backend("in-memory lock poisoned".into()))
    }

    /// Exclusive write access to the map (same poison handling as [`read_objects`]).
    fn write_objects(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, BTreeMap<String, Vec<u8>>>, StorageError> {
        self.objects
            .write()
            .map_err(|_| StorageError::Backend("in-memory lock poisoned".into()))
    }
}

#[async_trait]
impl StorageBackend for InMemoryBackend {
    async fn list(&self, dir: &str) -> Result<Vec<Entry>, StorageError> {
        let dir = path::normalize(dir);
        let prefix = path::to_key(&dir, true); // "" for root, "a/b/" otherwise
        let objects = self.read_objects()?;

        // The root always lists; a non-root dir must actually be a prefix.
        if !prefix.is_empty() && !is_prefix(&objects, &prefix) {
            // Distinguish "it's a file" from "doesn't exist".
            let file_key = path::to_key(&dir, false);
            return if objects.contains_key(&file_key) {
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
        for (key, bytes) in objects.iter() {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() {
                continue; // the directory marker for `prefix` itself
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
        let objects = self.read_objects()?;
        if let Some(bytes) = objects.get(&file_key) {
            return Ok(Entry::file(name, bytes.len() as u64));
        }
        let dir_prefix = path::to_key(&norm, true);
        if is_prefix(&objects, &dir_prefix) {
            return Ok(Entry::dir(name));
        }
        Err(StorageError::NotFound)
    }

    async fn read(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, StorageError> {
        let norm = path::normalize(path);
        let key = path::to_key(&norm, false);
        let objects = self.read_objects()?;
        let Some(bytes) = objects.get(&key) else {
            // A prefix with no object at the exact key is a directory.
            return if is_prefix(&objects, &path::to_key(&norm, true)) {
                Err(StorageError::NotAFile)
            } else {
                Err(StorageError::NotFound)
            };
        };
        let start = (offset as usize).min(bytes.len());
        let end = start.saturating_add(len).min(bytes.len());
        // Checked slice: `start <= end <= len` by construction, so this is
        // always `Some`, but `.get` keeps the code panic-free by construction.
        Ok(bytes.get(start..end).unwrap_or(&[]).to_vec())
    }

    async fn create(&self, path: &str, kind: EntryKind) -> Result<(), StorageError> {
        let norm = path::normalize(path);
        if norm == "/" {
            return Err(StorageError::AlreadyExists);
        }
        let file_key = path::to_key(&norm, false);
        let dir_prefix = path::to_key(&norm, true);
        let mut objects = self.write_objects()?;
        // Something already at this path (as a file, or as a directory prefix)?
        if objects.contains_key(&file_key) || is_prefix(&objects, &dir_prefix) {
            return Err(StorageError::AlreadyExists);
        }
        match kind {
            // An empty file is a zero-byte object; an empty directory is a
            // zero-byte marker key ending in `/`.
            EntryKind::File => objects.insert(file_key, Vec::new()),
            EntryKind::Dir => objects.insert(dir_prefix, Vec::new()),
        };
        Ok(())
    }

    async fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        let norm = path::normalize(path);
        let file_key = path::to_key(&norm, false);
        let dir_prefix = path::to_key(&norm, true);
        let mut objects = self.write_objects()?;
        // Reject a write to a path that resolves to a directory.
        if !objects.contains_key(&file_key) && is_prefix(&objects, &dir_prefix) {
            return Err(StorageError::NotAFile);
        }
        let buf = objects.entry(file_key).or_default();
        let offset = offset as usize;
        let end = offset.saturating_add(data.len());
        if buf.len() < end {
            buf.resize(end, 0); // zero-fill any gap before `offset`
        }
        if let Some(slot) = buf.get_mut(offset..end) {
            slot.copy_from_slice(data);
        }
        Ok(())
    }

    async fn truncate(&self, path: &str, len: u64) -> Result<(), StorageError> {
        let key = path::to_key(&path::normalize(path), false);
        let mut objects = self.write_objects()?;
        let Some(buf) = objects.get_mut(&key) else {
            return Err(StorageError::NotFound);
        };
        buf.resize(len as usize, 0);
        Ok(())
    }

    async fn remove(&self, path: &str, kind: EntryKind) -> Result<(), StorageError> {
        let norm = path::normalize(path);
        let mut objects = self.write_objects()?;
        match kind {
            EntryKind::File => {
                let key = path::to_key(&norm, false);
                if objects.remove(&key).is_none() {
                    return Err(StorageError::NotFound);
                }
            }
            EntryKind::Dir => {
                let prefix = path::to_key(&norm, true);
                // Any key strictly under the prefix means the dir isn't empty.
                if objects
                    .keys()
                    .any(|k| k.starts_with(&prefix) && k != &prefix)
                {
                    return Err(StorageError::NotEmpty);
                }
                // Drop the marker if there is one; a purely-implicit empty dir
                // can't exist (it would have no keys), so nothing here is NotFound.
                if objects.remove(&prefix).is_none() {
                    return Err(StorageError::NotFound);
                }
            }
        }
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), StorageError> {
        let from = path::normalize(from);
        let to = path::normalize(to);
        let from_file = path::to_key(&from, false);
        let to_file = path::to_key(&to, false);
        let mut objects = self.write_objects()?;

        // File rename: move the single key.
        if let Some(bytes) = objects.remove(&from_file) {
            objects.insert(to_file, bytes);
            return Ok(());
        }

        // Directory rename: re-key every object under the prefix.
        let from_dir = path::to_key(&from, true);
        let to_dir = path::to_key(&to, true);
        let keys: Vec<String> = objects
            .keys()
            .filter(|k| k.starts_with(&from_dir))
            .cloned()
            .collect();
        if keys.is_empty() {
            return Err(StorageError::NotFound);
        }
        for k in keys {
            if let Some(bytes) = objects.remove(&k) {
                let suffix = k.strip_prefix(&from_dir).unwrap_or(&k);
                objects.insert(format!("{to_dir}{suffix}"), bytes);
            }
        }
        Ok(())
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

    #[tokio::test]
    async fn create_write_read_roundtrip() {
        let b = InMemoryBackend::new();
        b.create("/note.txt", EntryKind::File).await.unwrap();
        assert_eq!(b.stat("/note.txt").await.unwrap().size, 0);
        b.write("/note.txt", 0, b"hello").await.unwrap();
        b.write("/note.txt", 5, b" world").await.unwrap();
        assert_eq!(b.stat("/note.txt").await.unwrap().size, 11);
        assert_eq!(b.read("/note.txt", 0, 100).await.unwrap(), b"hello world");
    }

    #[tokio::test]
    async fn write_past_end_zero_fills_the_gap() {
        let b = InMemoryBackend::new();
        b.write("/sparse.bin", 3, b"XY").await.unwrap();
        assert_eq!(b.read("/sparse.bin", 0, 100).await.unwrap(), b"\0\0\0XY");
    }

    #[tokio::test]
    async fn create_rejects_existing() {
        let b = sample();
        assert!(matches!(
            b.create("/readme.txt", EntryKind::File).await,
            Err(StorageError::AlreadyExists)
        ));
        assert!(matches!(
            b.create("/photos", EntryKind::Dir).await,
            Err(StorageError::AlreadyExists)
        ));
    }

    #[tokio::test]
    async fn truncate_shrinks_and_grows() {
        let b = sample();
        b.truncate("/readme.txt", 5).await.unwrap();
        assert_eq!(b.read("/readme.txt", 0, 100).await.unwrap(), b"hello");
        b.truncate("/readme.txt", 8).await.unwrap();
        assert_eq!(b.read("/readme.txt", 0, 100).await.unwrap(), b"hello\0\0\0");
        assert!(matches!(
            b.truncate("/nope", 0).await,
            Err(StorageError::NotFound)
        ));
    }

    #[tokio::test]
    async fn remove_file_and_empty_dir() {
        let b = sample();
        b.remove("/readme.txt", EntryKind::File).await.unwrap();
        assert!(matches!(
            b.stat("/readme.txt").await,
            Err(StorageError::NotFound)
        ));

        // A non-empty directory can't be removed.
        assert!(matches!(
            b.remove("/photos", EntryKind::Dir).await,
            Err(StorageError::NotEmpty)
        ));

        // An explicitly-created empty directory can.
        b.create("/empty", EntryKind::Dir).await.unwrap();
        assert!(b.stat("/empty").await.unwrap().is_dir());
        b.remove("/empty", EntryKind::Dir).await.unwrap();
        assert!(matches!(
            b.stat("/empty").await,
            Err(StorageError::NotFound)
        ));
    }

    #[tokio::test]
    async fn rename_file_and_directory() {
        let b = sample();
        b.rename("/readme.txt", "/README").await.unwrap();
        assert!(matches!(
            b.stat("/readme.txt").await,
            Err(StorageError::NotFound)
        ));
        assert_eq!(b.read("/README", 0, 100).await.unwrap(), b"hello world");

        // Renaming a directory moves its whole subtree.
        b.rename("/photos", "/pics").await.unwrap();
        assert!(matches!(
            b.stat("/photos").await,
            Err(StorageError::NotFound)
        ));
        assert_eq!(b.stat("/pics/2026/a.jpg").await.unwrap().size, 10);
        assert_eq!(b.stat("/pics/cover.png").await.unwrap().size, 5);

        assert!(matches!(
            b.rename("/missing", "/x").await,
            Err(StorageError::NotFound)
        ));
    }
}
