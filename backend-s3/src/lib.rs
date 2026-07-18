//! S3 (and S3-compatible) [`StorageBackend`](fskit_s3_core::StorageBackend).
//!
//! Read-only: `ListObjectsV2` + ranged `GetObject`, signed with a minimal
//! in-crate SigV4. Point it at AWS with [`S3Config::aws`], or at MinIO /
//! LocalStack / R2 by setting `endpoint` + `path_style`.

mod s3;
mod sigv4;

pub use s3::{S3Backend, S3Config};
pub use sigv4::Credentials;
