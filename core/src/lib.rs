//! Backend-agnostic core for the FSKit filesystem.
//!
//! FSKit hands the extension a small vocabulary of requests — "list this
//! directory", "stat this path", "read this byte range" — and does not care how
//! they are satisfied. That indifference is the seam: [`StorageBackend`] is the
//! whole contract, and the FSKit glue in the `ext` crate is written against the
//! trait, never against S3. The `fskit-s3-backend` crate implements it once over
//! Apache OpenDAL, so S3 is just the first enabled service; WebDAV/SFTP are a
//! feature flag away without touching the FSKit side.
//!
//! The trait is **async**. A network filesystem is latency-bound and Finder/
//! Photos issue many reads in parallel, so the ext holds a tokio runtime and
//! each FSKit operation `await`s the backend and fires its reply block on
//! completion — no queue thread is parked per in-flight read. `async-trait`
//! keeps the trait dyn-compatible so the ext can hold an `Arc<dyn StorageBackend>`.

// Library code must never panic: no unwrap/expect/panic/indexing outside tests.
// Enforced by clippy in CI; tests may still unwrap freely.
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

use std::error::Error;
use std::fmt;
use std::time::SystemTime;

pub use async_trait::async_trait;

pub mod path;

#[cfg(any(test, feature = "mem"))]
pub mod mem;

/// Whether a directory entry is a file or a subdirectory.
///
/// Object stores have no real directories — a "directory" is any common prefix
/// of the flat key space. Backends synthesize [`EntryKind::Dir`] from prefixes;
/// the FSKit side never needs to know the difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Dir,
}

/// One item in a directory listing, or the result of a stat.
///
/// `name` is always a single path component (the basename), never a full path —
/// this is what FSKit enumerates children by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Basename, e.g. `photo.jpg` or `subdir`. Never contains `/`.
    pub name: String,
    pub kind: EntryKind,
    /// Size in bytes. `0` for directories.
    pub size: u64,
    /// Last-modified time if the backend reports one.
    pub modified: Option<SystemTime>,
    /// Opaque version tag if the backend has one (S3 ETag, etc.). Useful later
    /// for cache invalidation; unused by the FSKit mapping today.
    pub etag: Option<String>,
}

impl Entry {
    pub fn file(name: impl Into<String>, size: u64) -> Self {
        Entry {
            name: name.into(),
            kind: EntryKind::File,
            size,
            modified: None,
            etag: None,
        }
    }

    pub fn dir(name: impl Into<String>) -> Self {
        Entry {
            name: name.into(),
            kind: EntryKind::Dir,
            size: 0,
            modified: None,
            etag: None,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

/// Errors a backend can surface. The `ext` crate maps each variant onto the
/// corresponding `FSKitError`/POSIX errno so the kernel sees a sane result
/// (`NotFound` → `ENOENT`, `NotADirectory` → `ENOTDIR`, …).
#[derive(Debug)]
pub enum StorageError {
    /// No object or prefix exists at the path.
    NotFound,
    /// A file operation targeted something that is a directory.
    NotAFile,
    /// A directory operation targeted something that is a file.
    NotADirectory,
    /// A create targeted a path that already exists.
    AlreadyExists,
    /// A directory removal targeted a directory that still has children.
    NotEmpty,
    /// The path or a request parameter was malformed.
    InvalidPath(String),
    /// Anything backend-specific: HTTP failure, auth error, XML parse, I/O.
    Backend(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::NotFound => write!(f, "not found"),
            StorageError::NotAFile => write!(f, "not a file"),
            StorageError::NotADirectory => write!(f, "not a directory"),
            StorageError::AlreadyExists => write!(f, "already exists"),
            StorageError::NotEmpty => write!(f, "directory not empty"),
            StorageError::InvalidPath(p) => write!(f, "invalid path: {p}"),
            StorageError::Backend(msg) => write!(f, "backend error: {msg}"),
        }
    }
}

impl Error for StorageError {}

/// The one contract the FSKit extension is written against.
///
/// Paths are absolute, `/`-separated, and normalized (see [`path`]): the root is
/// `"/"`, no trailing slash otherwise, no `.`/`..` components. A backend may
/// assume it receives already-normalized paths from the `ext` crate.
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Immediate children of a directory. `dir` is `"/"` for the root.
    ///
    /// Returns [`StorageError::NotFound`] if the directory does not exist and
    /// [`StorageError::NotADirectory`] if the path is a file.
    async fn list(&self, dir: &str) -> Result<Vec<Entry>, StorageError>;

    /// Metadata for a single path. The returned [`Entry::name`] is the basename
    /// of `path` (the root reports itself as a directory named `""`).
    async fn stat(&self, path: &str) -> Result<Entry, StorageError>;

    /// Read up to `len` bytes starting at `offset` from a file.
    ///
    /// A short read (fewer than `len` bytes) signals end-of-file and is not an
    /// error. Returns [`StorageError::NotAFile`] for a directory.
    async fn read(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, StorageError>;

    // ---- mutating operations ------------------------------------------------
    //
    // Object stores have no partial-write, append, or atomic-rename primitives:
    // a key is written or copied whole. So `write`/`truncate` are read-modify-
    // write of the entire object and `rename` is copy-then-delete. That is
    // O(object size) per call (and O(n²) for a large file written in many small
    // chunks) — correct and simple, the deliberate first cut. A future
    // optimization can buffer a file's writes per open handle and flush once.

    /// Create an empty file, or a directory, at `path`.
    ///
    /// Returns [`StorageError::AlreadyExists`] if a file or directory already
    /// exists there.
    async fn create(&self, path: &str, kind: EntryKind) -> Result<(), StorageError>;

    /// Write `data` starting at `offset`, extending the file (zero-filling any
    /// gap between the old end and `offset`) as needed. The whole slice is
    /// written on success. Returns [`StorageError::NotAFile`] for a directory.
    async fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), StorageError>;

    /// Truncate or zero-extend a file to exactly `len` bytes.
    async fn truncate(&self, path: &str, len: u64) -> Result<(), StorageError>;

    /// Remove a file, or an empty directory, at `path`. `kind` says which is
    /// expected (the FSKit side always knows from the item it holds).
    ///
    /// Returns [`StorageError::NotEmpty`] if `path` is a directory that still
    /// has children.
    async fn remove(&self, path: &str, kind: EntryKind) -> Result<(), StorageError>;

    /// Rename `from` to `to`, moving the whole subtree if `from` is a directory.
    /// An existing file at `to` is overwritten.
    async fn rename(&self, from: &str, to: &str) -> Result<(), StorageError>;
}
