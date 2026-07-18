//! Backend-agnostic core for the FSKit filesystem.
//!
//! FSKit hands the extension a small vocabulary of requests — "list this
//! directory", "stat this path", "read this byte range" — and does not care how
//! they are satisfied. That indifference is the seam: [`StorageBackend`] is the
//! whole contract, and the FSKit glue in the `ext` crate is written against the
//! trait, never against S3. S3 is merely the first implementor
//! (`fskit-s3-backend-s3`); WebDAV/SSH can be added later as new crates without
//! touching the FSKit side.
//!
//! The trait is intentionally **blocking**. FSKit invokes volume operations on
//! its own dispatch queues and wants a reply block called when the work is done,
//! so a backend that blocks on network I/O maps directly onto that model without
//! dragging an async runtime into the extension. If a backend needs concurrency
//! it can manage its own internally.

use std::error::Error;
use std::fmt;
use std::time::SystemTime;

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
        Entry { name: name.into(), kind: EntryKind::File, size, modified: None, etag: None }
    }

    pub fn dir(name: impl Into<String>) -> Self {
        Entry { name: name.into(), kind: EntryKind::Dir, size: 0, modified: None, etag: None }
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
pub trait StorageBackend: Send + Sync {
    /// Immediate children of a directory. `dir` is `"/"` for the root.
    ///
    /// Returns [`StorageError::NotFound`] if the directory does not exist and
    /// [`StorageError::NotADirectory`] if the path is a file.
    fn list(&self, dir: &str) -> Result<Vec<Entry>, StorageError>;

    /// Metadata for a single path. The returned [`Entry::name`] is the basename
    /// of `path` (the root reports itself as a directory named `""`).
    fn stat(&self, path: &str) -> Result<Entry, StorageError>;

    /// Read up to `len` bytes starting at `offset` from a file.
    ///
    /// A short read (fewer than `len` bytes) signals end-of-file and is not an
    /// error. Returns [`StorageError::NotAFile`] for a directory.
    fn read(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, StorageError>;
}
