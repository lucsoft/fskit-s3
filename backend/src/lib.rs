//! The concrete [`StorageBackend`](fskit_s3_core::StorageBackend), implemented
//! once over [Apache OpenDAL](https://opendal.apache.org).
//!
//! OpenDAL already abstracts ~40 storage services behind one `Operator`, so the
//! S3 client, request signing, XML parsing, retries, and pagination are all
//! OpenDAL's job, not ours. Adding WebDAV or SFTP later is a new `services-*`
//! feature plus a constructor — [`OpenDalBackend`] and the whole FSKit side stay
//! unchanged because everything goes through the trait.
//!
//! The adapter is generic over the `Operator`, which is what makes it testable:
//! the unit tests drive it against OpenDAL's in-memory service and exercise the
//! real list/stat/read semantics without a live bucket.

// Library code must never panic outside tests (enforced by clippy in CI).
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::unreachable
    )
)]

use fskit_s3_core::{async_trait, path, Entry, EntryKind, StorageBackend, StorageError};
use opendal::{EntryMode, ErrorKind, Operator};

/// Connection details for an S3 (or S3-compatible) bucket.
#[derive(Clone, Debug, Default)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// Custom endpoint for S3-compatible stores (MinIO, R2, LocalStack). Empty
    /// ⇒ OpenDAL's AWS default.
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// STS/assumed-role session token, if any.
    pub session_token: Option<String>,
}

/// A [`StorageBackend`] over any OpenDAL `Operator`.
pub struct OpenDalBackend {
    op: Operator,
}

impl OpenDalBackend {
    /// Wrap an already-built `Operator` (used by tests with the memory service,
    /// and by any future backend constructor).
    pub fn new(op: Operator) -> Self {
        OpenDalBackend { op }
    }

    /// Build an S3-backed instance from [`S3Config`].
    pub fn s3(cfg: &S3Config) -> Result<Self, StorageError> {
        let mut builder = opendal::services::S3::default()
            .bucket(&cfg.bucket)
            .access_key_id(&cfg.access_key_id)
            .secret_access_key(&cfg.secret_access_key);
        if !cfg.region.is_empty() {
            builder = builder.region(&cfg.region);
        }
        if !cfg.endpoint.is_empty() {
            builder = builder.endpoint(&cfg.endpoint);
        }
        if let Some(token) = &cfg.session_token {
            builder = builder.session_token(token);
        }
        let op = Operator::new(builder).map_err(map_err)?.finish();
        Ok(OpenDalBackend { op })
    }
}

/// Map an OpenDAL error onto the trait's error vocabulary.
fn map_err(e: opendal::Error) -> StorageError {
    match e.kind() {
        ErrorKind::NotFound => StorageError::NotFound,
        ErrorKind::IsADirectory => StorageError::NotAFile,
        ErrorKind::NotADirectory => StorageError::NotADirectory,
        ErrorKind::AlreadyExists => StorageError::AlreadyExists,
        _ => StorageError::Backend(e.to_string()),
    }
}

/// The object's last-modified time as a `SystemTime`, if the service reports one
/// (S3 always does). A *stable* mtime matters: the ext maps it onto the FSKit
/// modify timestamp, and editors (vim's "changed since reading it!!!") warn when
/// an mtime advances between opening and saving — which it did when we reported
/// `now()`. Converted via the inherent chrono accessors so no `chrono` dep is
/// needed; a pre-epoch time (never real for an object store) is dropped.
fn meta_modified(meta: &opendal::Metadata) -> Option<std::time::SystemTime> {
    let dt = meta.last_modified()?;
    let secs = u64::try_from(dt.timestamp()).ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::new(secs, dt.timestamp_subsec_nanos()))
}

impl OpenDalBackend {
    /// Copy one object, server-side when the service supports it (S3 does),
    /// falling back to a client-side read+write for services that don't (so
    /// `rename` works everywhere the trait is used, e.g. the memory service).
    async fn copy_object(&self, from: &str, to: &str) -> Result<(), StorageError> {
        match self.op.copy(from, to).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::Unsupported => {
                let buf = self.op.read(from).await.map_err(map_err)?;
                self.op.write(to, buf.to_vec()).await.map_err(map_err)?;
                Ok(())
            }
            Err(e) => Err(map_err(e)),
        }
    }
}

#[async_trait]
impl StorageBackend for OpenDalBackend {
    async fn list(&self, dir: &str) -> Result<Vec<Entry>, StorageError> {
        let dir = path::normalize(dir);
        let prefix = path::to_key(&dir, true); // "" at root, "a/b/" otherwise

        // Non-recursive list: files come back plain, subdirectories as entries
        // whose path ends in '/'. OpenDAL applies the S3 delimiter for us.
        let entries = self.op.list(&prefix).await.map_err(map_err)?;

        let mut out = Vec::with_capacity(entries.len());
        let mut saw_self = false;
        for entry in entries {
            // OpenDAL includes the listed directory itself; its presence proves
            // the prefix exists even when it has no children (an empty dir marker).
            if entry.path() == prefix {
                saw_self = true;
                continue;
            }
            let name = entry.name().trim_end_matches('/').to_string();
            if name.is_empty() {
                continue;
            }
            let meta = entry.metadata();
            out.push(match meta.mode() {
                EntryMode::DIR => Entry::dir(name),
                _ => {
                    let mut e = Entry::file(name, meta.content_length());
                    e.modified = meta_modified(meta);
                    e
                }
            });
        }

        // A non-root prefix with neither children nor a marker doesn't exist.
        if !prefix.is_empty() && out.is_empty() && !saw_self {
            return Err(StorageError::NotFound);
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn stat(&self, p: &str) -> Result<Entry, StorageError> {
        let norm = path::normalize(p);
        if norm == "/" {
            return Ok(Entry::dir(""));
        }
        let name = path::basename(&norm).to_string();
        let file_key = path::to_key(&norm, false);

        match self.op.stat(&file_key).await {
            Ok(meta) if meta.mode() == EntryMode::FILE => {
                let mut e = Entry::file(name, meta.content_length());
                e.modified = meta_modified(&meta);
                return Ok(e);
            }
            Ok(_) => return Ok(Entry::dir(name)), // stat resolved it as a directory
            Err(e) if e.kind() == ErrorKind::NotFound => {} // maybe a prefix; check below
            Err(e) => return Err(map_err(e)),
        }

        // No object at the exact key: is it a directory prefix?
        match self.list(&norm).await {
            Ok(_) => Ok(Entry::dir(name)),
            Err(e) => Err(e),
        }
    }

    async fn read(&self, p: &str, offset: u64, len: usize) -> Result<Vec<u8>, StorageError> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let norm = path::normalize(p);
        let key = path::to_key(&norm, false);

        // Clamp the range to the object size: OpenDAL slices the response buffer
        // by the requested range and panics if the end exceeds the object, and a
        // read past EOF must be a short read, not an error. This costs a stat per
        // read for now; the ext can avoid it later by carrying the size it
        // already learned via getAttributes on the FSItem.
        let size = match self.op.stat(&key).await {
            Ok(meta) if meta.mode() == EntryMode::FILE => meta.content_length(),
            Ok(_) => return Err(StorageError::NotAFile),
            Err(e) => return Err(map_err(e)),
        };
        if offset >= size {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(len as u64).min(size);
        let buf = self
            .op
            .read_with(&key)
            .range(offset..end)
            .await
            .map_err(map_err)?;
        Ok(buf.to_vec())
    }

    async fn create(&self, p: &str, kind: EntryKind) -> Result<(), StorageError> {
        let norm = path::normalize(p);
        if norm == "/" {
            return Err(StorageError::AlreadyExists);
        }
        // Refuse to clobber anything already at this path (as a file or a dir).
        if self.stat(&norm).await.is_ok() {
            return Err(StorageError::AlreadyExists);
        }
        match kind {
            // An empty file is a zero-byte object; a directory is a prefix, which
            // OpenDAL materializes via create_dir (a key ending in `/`).
            EntryKind::File => {
                self.op
                    .write(&path::to_key(&norm, false), Vec::<u8>::new())
                    .await
                    .map_err(map_err)?;
            }
            EntryKind::Dir => {
                self.op
                    .create_dir(&path::to_key(&norm, true))
                    .await
                    .map_err(map_err)?;
            }
        }
        Ok(())
    }

    async fn write(&self, p: &str, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        // Object stores have no partial write: read the whole object, splice the
        // new bytes in (zero-filling any gap), and put it back. See the trait doc.
        let norm = path::normalize(p);
        let key = path::to_key(&norm, false);
        let mut buf = match self.op.read(&key).await {
            Ok(b) => b.to_vec(),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                // No object at the exact key: a directory prefix, or a brand-new
                // file. Reject the former; treat the latter as an empty base.
                if self.stat(&norm).await.map(|e| e.is_dir()).unwrap_or(false) {
                    return Err(StorageError::NotAFile);
                }
                Vec::new()
            }
            Err(e) => return Err(map_err(e)), // IsADirectory -> NotAFile, etc.
        };
        let offset = offset as usize;
        let end = offset.saturating_add(data.len());
        if buf.len() < end {
            buf.resize(end, 0);
        }
        if let Some(slot) = buf.get_mut(offset..end) {
            slot.copy_from_slice(data);
        }
        self.op.write(&key, buf).await.map_err(map_err)?;
        Ok(())
    }

    async fn truncate(&self, p: &str, len: u64) -> Result<(), StorageError> {
        let key = path::to_key(&path::normalize(p), false);
        let mut buf = match self.op.read(&key).await {
            Ok(b) => b.to_vec(),
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(StorageError::NotFound),
            Err(e) => return Err(map_err(e)),
        };
        buf.resize(len as usize, 0);
        self.op.write(&key, buf).await.map_err(map_err)?;
        Ok(())
    }

    async fn remove(&self, p: &str, kind: EntryKind) -> Result<(), StorageError> {
        let norm = path::normalize(p);
        match kind {
            EntryKind::File => {
                let key = path::to_key(&norm, false);
                match self.op.stat(&key).await {
                    Ok(_) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => {
                        return Err(StorageError::NotFound)
                    }
                    Err(e) => return Err(map_err(e)),
                }
                self.op.delete(&key).await.map_err(map_err)?;
            }
            EntryKind::Dir => {
                // A non-empty directory can't be removed; `list` also surfaces
                // NotFound for a directory that doesn't exist at all.
                if !self.list(&norm).await?.is_empty() {
                    return Err(StorageError::NotEmpty);
                }
                // Drop the prefix marker. delete is idempotent, so a purely
                // implicit (marker-less) empty prefix is a harmless no-op.
                self.op
                    .delete(&path::to_key(&norm, true))
                    .await
                    .map_err(map_err)?;
            }
        }
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), StorageError> {
        let from = path::normalize(from);
        let to = path::normalize(to);
        let from_file = path::to_key(&from, false);
        let to_file = path::to_key(&to, false);

        // File rename: copy the single object, then drop the original.
        match self.op.stat(&from_file).await {
            Ok(meta) if meta.mode() == EntryMode::FILE => {
                self.copy_object(&from_file, &to_file).await?;
                self.op.delete(&from_file).await.map_err(map_err)?;
                return Ok(());
            }
            Ok(_) => {}                                     // a dir object; handle below
            Err(e) if e.kind() == ErrorKind::NotFound => {} // maybe a prefix dir
            Err(e) => return Err(map_err(e)),
        }

        // Directory rename: re-key every object under the prefix.
        let from_dir = path::to_key(&from, true);
        let to_dir = path::to_key(&to, true);
        let entries = self
            .op
            .list_with(&from_dir)
            .recursive(true)
            .await
            .map_err(map_err)?;
        let mut moved = 0usize;
        for entry in entries {
            let src = entry.path();
            let Some(suffix) = src.strip_prefix(&from_dir) else {
                continue;
            };
            if suffix.is_empty() {
                continue; // the prefix directory itself
            }
            let dst = format!("{to_dir}{suffix}");
            if entry.metadata().mode() == EntryMode::DIR {
                self.op.create_dir(&dst).await.map_err(map_err)?;
            } else {
                self.copy_object(src, &dst).await?;
            }
            self.op.delete(src).await.map_err(map_err)?;
            moved += 1;
        }
        if moved == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an in-memory OpenDAL operator seeded with a small object tree.
    async fn sample() -> OpenDalBackend {
        let op = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        op.write("readme.txt", "hello world".as_bytes())
            .await
            .unwrap();
        op.write("photos/2026/a.jpg", vec![0u8; 10]).await.unwrap();
        op.write("photos/2026/b.jpg", vec![0u8; 20]).await.unwrap();
        op.write("photos/cover.png", vec![0u8; 5]).await.unwrap();
        OpenDalBackend::new(op)
    }

    #[tokio::test]
    async fn root_lists_files_and_synth_dirs() {
        let b = sample().await;
        let names: Vec<_> = b
            .list("/")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["photos", "readme.txt"]);
    }

    #[tokio::test]
    async fn nested_listing_with_sizes() {
        let b = sample().await;
        let names: Vec<_> = b
            .list("/photos")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["2026", "cover.png"]);

        // list gives names + kinds; sizes come from stat (FSKit's model, and
        // robust across services that don't return sizes in listings).
        let deep: Vec<_> = b
            .list("/photos/2026")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(deep, vec!["a.jpg", "b.jpg"]);
        assert_eq!(b.stat("/photos/2026/a.jpg").await.unwrap().size, 10);
        assert_eq!(b.stat("/photos/2026/b.jpg").await.unwrap().size, 20);
    }

    #[tokio::test]
    async fn stat_file_dir_and_missing() {
        let b = sample().await;
        assert!(b.stat("/").await.unwrap().is_dir());
        assert!(b.stat("/photos").await.unwrap().is_dir());
        assert_eq!(b.stat("/readme.txt").await.unwrap().size, 11);
        assert!(matches!(b.stat("/nope").await, Err(StorageError::NotFound)));
    }

    #[test]
    fn meta_modified_converts_and_clamps() {
        use opendal::{EntryMode, Metadata};
        // A real last-modified time round-trips to the expected SystemTime; the
        // ext maps this onto a *stable* FSKit mtime (fixes the editor "changed
        // since reading it" warning that a per-call `now()` caused).
        let dt = chrono::DateTime::from_timestamp(1_700_000_123, 456).unwrap();
        let meta = Metadata::new(EntryMode::FILE).with_last_modified(dt);
        let expected = std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_123, 456);
        assert_eq!(meta_modified(&meta), Some(expected));

        // No time reported (OpenDAL's memory service, dir prefixes) => None, so
        // the ext falls back to its process-stable instant.
        assert_eq!(meta_modified(&Metadata::new(EntryMode::FILE)), None);

        // A pre-epoch time (never real for an object store) is dropped, not panicked.
        let pre = chrono::DateTime::from_timestamp(-5, 0).unwrap();
        assert_eq!(
            meta_modified(&Metadata::new(EntryMode::FILE).with_last_modified(pre)),
            None
        );
    }

    #[tokio::test]
    async fn memory_service_reports_no_modified_time() {
        // Documents why the ext needs a fallback: OpenDAL's in-memory service (the
        // demo mount) carries no last-modified, so `Entry.modified` is None there.
        let b = sample().await;
        assert!(b.stat("/readme.txt").await.unwrap().modified.is_none());
    }

    #[tokio::test]
    async fn ranged_reads() {
        let b = sample().await;
        assert_eq!(b.read("/readme.txt", 0, 5).await.unwrap(), b"hello");
        assert_eq!(b.read("/readme.txt", 6, 100).await.unwrap(), b"world");
        assert!(matches!(
            b.read("/missing", 0, 4).await,
            Err(StorageError::NotFound)
        ));
    }

    fn empty() -> OpenDalBackend {
        let op = Operator::new(opendal::services::Memory::default())
            .unwrap()
            .finish();
        OpenDalBackend::new(op)
    }

    #[tokio::test]
    async fn create_write_read_roundtrip() {
        let b = empty();
        b.create("/note.txt", EntryKind::File).await.unwrap();
        assert_eq!(b.stat("/note.txt").await.unwrap().size, 0);
        b.write("/note.txt", 0, b"hello").await.unwrap();
        b.write("/note.txt", 5, b" world").await.unwrap();
        assert_eq!(b.stat("/note.txt").await.unwrap().size, 11);
        assert_eq!(b.read("/note.txt", 0, 100).await.unwrap(), b"hello world");
    }

    #[tokio::test]
    async fn write_past_end_zero_fills_the_gap() {
        let b = empty();
        b.write("/sparse.bin", 3, b"XY").await.unwrap();
        assert_eq!(b.read("/sparse.bin", 0, 100).await.unwrap(), b"\0\0\0XY");
    }

    #[tokio::test]
    async fn create_rejects_existing() {
        let b = sample().await;
        assert!(matches!(
            b.create("/readme.txt", EntryKind::File).await,
            Err(StorageError::AlreadyExists)
        ));
    }

    #[tokio::test]
    async fn truncate_shrinks_and_grows() {
        let b = sample().await;
        b.truncate("/readme.txt", 5).await.unwrap();
        assert_eq!(b.read("/readme.txt", 0, 100).await.unwrap(), b"hello");
        b.truncate("/readme.txt", 8).await.unwrap();
        assert_eq!(b.stat("/readme.txt").await.unwrap().size, 8);
        assert!(matches!(
            b.truncate("/nope", 0).await,
            Err(StorageError::NotFound)
        ));
    }

    #[tokio::test]
    async fn remove_file_and_dirs() {
        let b = sample().await;
        b.remove("/readme.txt", EntryKind::File).await.unwrap();
        assert!(matches!(
            b.stat("/readme.txt").await,
            Err(StorageError::NotFound)
        ));
        assert!(matches!(
            b.remove("/missing", EntryKind::File).await,
            Err(StorageError::NotFound)
        ));
        // photos/ still has children.
        assert!(matches!(
            b.remove("/photos", EntryKind::Dir).await,
            Err(StorageError::NotEmpty)
        ));

        let b = empty();
        b.create("/d", EntryKind::Dir).await.unwrap();
        assert!(b.stat("/d").await.unwrap().is_dir());
        b.remove("/d", EntryKind::Dir).await.unwrap();
        assert!(matches!(b.stat("/d").await, Err(StorageError::NotFound)));
    }

    #[tokio::test]
    async fn rename_file_and_directory() {
        let b = sample().await;
        b.rename("/readme.txt", "/README").await.unwrap();
        assert!(matches!(
            b.stat("/readme.txt").await,
            Err(StorageError::NotFound)
        ));
        assert_eq!(b.read("/README", 0, 100).await.unwrap(), b"hello world");

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

    /// End-to-end against a live S3 endpoint (the `compose.yaml` RustFS). Ignored
    /// by default so CI stays hermetic; run it with:
    ///
    /// ```sh
    /// docker compose up -d
    /// RUSTFS_ENDPOINT=http://localhost:9000 cargo test -p fskit-s3-backend -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "requires `docker compose up`; set RUSTFS_ENDPOINT and run with --ignored"]
    async fn live_s3_roundtrip() {
        let Ok(endpoint) = std::env::var("RUSTFS_ENDPOINT") else {
            eprintln!("RUSTFS_ENDPOINT unset; skipping live test");
            return;
        };
        let cfg = S3Config {
            bucket: "test-bucket".into(),
            region: "us-east-1".into(),
            endpoint,
            access_key_id: "fskit".into(),
            secret_access_key: "fskit-secret".into(),
            session_token: None,
        };
        let backend = OpenDalBackend::s3(&cfg).expect("build s3 backend");

        // compose seeds test-bucket/hello.txt = "hello from rustfs\n".
        let root = backend.list("/").await.expect("list root");
        assert!(root.iter().any(|e| e.name == "hello.txt"), "root: {root:?}");
        let hello = backend.stat("/hello.txt").await.expect("stat");
        assert_eq!(hello.size, 18);
        // Real S3 reports a last-modified time; the ext maps it to a stable mtime.
        assert!(
            hello.modified.is_some(),
            "S3 stat should carry a modified time"
        );
        assert_eq!(
            backend.read("/hello.txt", 0, 1024).await.expect("read"),
            b"hello from rustfs\n"
        );

        // Write path: create → write → truncate → rename → read → remove, on a
        // scratch key so the test leaves the bucket as it found it.
        backend
            .create("/scratch.txt", EntryKind::File)
            .await
            .unwrap();
        backend.write("/scratch.txt", 0, b"hello").await.unwrap();
        backend.write("/scratch.txt", 5, b" there!").await.unwrap();
        assert_eq!(backend.stat("/scratch.txt").await.unwrap().size, 12);
        backend.truncate("/scratch.txt", 5).await.unwrap();
        backend
            .rename("/scratch.txt", "/scratch2.txt")
            .await
            .unwrap();
        assert_eq!(
            backend.read("/scratch2.txt", 0, 1024).await.unwrap(),
            b"hello"
        );
        backend
            .remove("/scratch2.txt", EntryKind::File)
            .await
            .unwrap();
        assert!(matches!(
            backend.stat("/scratch2.txt").await,
            Err(StorageError::NotFound)
        ));
    }
}
