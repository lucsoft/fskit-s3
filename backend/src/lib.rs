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

use fskit_s3_core::{async_trait, path, Entry, StorageBackend, StorageError};
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
        _ => StorageError::Backend(e.to_string()),
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
        for entry in entries {
            // OpenDAL includes the listed directory itself; skip it.
            if entry.path() == prefix {
                continue;
            }
            let name = entry.name().trim_end_matches('/').to_string();
            if name.is_empty() {
                continue;
            }
            let meta = entry.metadata();
            out.push(match meta.mode() {
                EntryMode::DIR => Entry::dir(name),
                _ => Entry::file(name, meta.content_length()),
            });
        }

        // A non-root prefix that lists nothing doesn't exist.
        if !prefix.is_empty() && out.is_empty() {
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
                return Ok(Entry::file(name, meta.content_length()));
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
        assert_eq!(backend.stat("/hello.txt").await.expect("stat").size, 18);
        assert_eq!(
            backend.read("/hello.txt", 0, 1024).await.expect("read"),
            b"hello from rustfs\n"
        );
    }
}
