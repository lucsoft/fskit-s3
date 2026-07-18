//! "Test and Save" credential check for S3 connections.
//!
//! Builds the OpenDAL S3 backend from a connection's [`S3Meta`] + the entered
//! secret and lists the bucket root once, so the form can confirm the credentials
//! and endpoint work before saving. Runs the async backend on a short-lived
//! runtime — the same `fskit-s3-backend` the extension serves with.

use fskit_s3_backend::{OpenDalBackend, S3Config};
use fskit_s3_core::StorageBackend;

use crate::connection::S3Meta;

/// Validate S3 credentials by listing the bucket root. `Ok(())` means the backend
/// built and the listing succeeded; `Err` carries a human-readable reason.
pub fn test_s3(meta: &S3Meta, secret: &str) -> Result<(), String> {
    let cfg = S3Config {
        bucket: meta.bucket.clone(),
        region: meta.region.clone(),
        endpoint: meta.endpoint.clone(),
        access_key_id: meta.access_key_id.clone(),
        secret_access_key: secret.to_string(),
        session_token: meta.session_token.clone(),
    };
    let backend = OpenDalBackend::s3(&cfg).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(backend.list("/"))
        .map(|_| ())
        .map_err(|e| e.to_string())
}
