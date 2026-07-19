//! Live integration tests against a real S3-compatible endpoint.
//!
//! These drive [`OpenDalBackend::s3`] — the *exact* backend the FSKit extension
//! serves with — against a running S3 service, rather than OpenDAL's in-memory
//! service the unit tests in `src/lib.rs` use. A live endpoint is the only place
//! two things can be checked end to end:
//!
//! * the **modified-time** behavior the ext depends on (the in-memory service
//!   reports no `last_modified` at all), and
//! * S3's **server-side copy** on `rename` (the in-memory service falls back to a
//!   client-side read+write, so that code path is never taken there).
//!
//! They are `#[ignore]`d so `cargo test` stays hermetic, and additionally skip
//! themselves (printing a note) when `RUSTFS_ENDPOINT` is unset — so a blanket
//! `cargo test -- --ignored` on a machine without an endpoint is harmless.
//!
//! Run against the local RustFS from `compose.yaml`:
//!
//! ```sh
//! docker compose up -d
//! RUSTFS_ENDPOINT=http://localhost:9000 \
//!   cargo test -p fskit-s3-backend --test live_s3 -- --ignored --nocapture
//! ```
//!
//! Point them at any other S3 (real AWS, MinIO, R2, …) by overriding the
//! `FSKIT_S3_*` env vars below and setting `RUSTFS_ENDPOINT` to its URL.
//!
//! Every test works under its own unique key prefix and deletes what it writes,
//! so runs are isolated from each other and leave the bucket as they found it.
//! In particular they do **not** rely on any seeded object: real use churns the
//! bucket (an FSKit mount + an editor leaves swap files, `4913` probes, …), so a
//! test that assumed a pristine `hello.txt` would be flaky. A run that panics
//! partway leaves one uniquely-named object behind; the unique prefix keeps that
//! from affecting later runs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fskit_s3_backend::{OpenDalBackend, S3Config};
use fskit_s3_core::{EntryKind, StorageBackend, StorageError};

/// An env var's value, or `default` when it is unset.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Build the backend from the environment, or `None` (printing why) when
/// `RUSTFS_ENDPOINT` is unset, so the caller can skip. Credentials/bucket default
/// to the `compose.yaml` RustFS and are overridable via `FSKIT_S3_*`.
fn backend() -> Option<OpenDalBackend> {
    let Ok(endpoint) = std::env::var("RUSTFS_ENDPOINT") else {
        eprintln!("RUSTFS_ENDPOINT unset; skipping live S3 test");
        return None;
    };
    let cfg = S3Config {
        bucket: env_or("FSKIT_S3_BUCKET", "test-bucket"),
        region: env_or("FSKIT_S3_REGION", "us-east-1"),
        endpoint,
        access_key_id: env_or("FSKIT_S3_ACCESS_KEY_ID", "fskit"),
        secret_access_key: env_or("FSKIT_S3_SECRET_ACCESS_KEY", "fskit-secret"),
        session_token: None,
    };
    Some(OpenDalBackend::s3(&cfg).expect("build S3 backend from env config"))
}

/// A unique, collision-free absolute path for one test invocation, so parallel
/// tests and leftovers from previous (possibly failed) runs never interfere. The
/// atomic counter makes it unique within a process even if the clock is coarse;
/// the timestamp makes it unique across runs.
fn unique_path(test: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("/fskit-s3-it/{test}-{}-{nanos}-{n}.txt", std::process::id())
}

/// The exact lifecycle the request describes: create a file, update it, update
/// it again, check its modified state and every reported stat, then delete it.
#[tokio::test]
#[ignore = "requires a live S3 endpoint; set RUSTFS_ENDPOINT and run with --ignored"]
async fn file_lifecycle_create_update_update_stat_delete() {
    let Some(b) = backend() else { return };
    let path = unique_path("lifecycle");
    // Compute the basename independently of the backend so the `name` assertion
    // is a real check, not a tautology against the backend's own path helpers.
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));

    // --- create: a brand-new, empty file -------------------------------------
    b.create(&path, EntryKind::File).await.expect("create");
    let created = b.stat(&path).await.expect("stat after create");
    assert_eq!(created.name, name, "stat reports the basename");
    assert_eq!(created.kind, EntryKind::File);
    assert_eq!(created.size, 0, "a freshly created file is empty");
    assert!(
        created.modified.is_some(),
        "real S3 reports a last-modified time (the in-memory service does not)"
    );

    // The new file is enumerable in its directory (FSKit's enumerate step). Use
    // `find`, not an equality check: the shared prefix may hold other tests'
    // files and leftovers from earlier runs.
    let listing = b.list(parent).await.expect("list parent");
    let listed = listing
        .iter()
        .find(|e| e.name == name)
        .expect("the created file appears in its directory listing");
    assert_eq!(listed.kind, EntryKind::File);

    // --- first update --------------------------------------------------------
    b.write(&path, 0, b"hello").await.expect("first write");
    let v1 = b.stat(&path).await.expect("stat after first write");
    assert_eq!(v1.size, 5);
    assert_eq!(b.read(&path, 0, 4096).await.expect("read v1"), b"hello");

    // Space the two updates apart: S3's `Last-Modified` has one-second
    // granularity on real AWS (RustFS is finer), so back-to-back writes could
    // otherwise share an mtime and the advance assertion below would be flaky.
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // --- second update -------------------------------------------------------
    b.write(&path, 5, b" world").await.expect("second write");
    let v2 = b.stat(&path).await.expect("stat after second write");
    assert_eq!(v2.size, 11);
    assert_eq!(
        b.read(&path, 0, 4096).await.expect("read v2"),
        b"hello world"
    );

    // --- modified state advanced across the (spaced) update ------------------
    let m1 = v1.modified.expect("v1 reports an mtime");
    let m2 = v2.modified.expect("v2 reports an mtime");
    assert!(
        m2 > m1,
        "mtime must advance after a modification (v1={m1:?}, v2={m2:?})"
    );

    // --- delete --------------------------------------------------------------
    b.remove(&path, EntryKind::File).await.expect("remove");
    assert!(
        matches!(b.stat(&path).await, Err(StorageError::NotFound)),
        "the file is gone after remove"
    );
}

/// The mtime the backend reports must be **stable** while the object is
/// untouched. The ext maps it onto FSKit's modify timestamp, and a value that
/// drifted between stats (e.g. a per-call `now()`) makes editors warn "the file
/// has been changed since reading it!!!" and abort saves. A live endpoint is the
/// only place this holds real data — the in-memory service reports no mtime.
#[tokio::test]
#[ignore = "requires a live S3 endpoint; set RUSTFS_ENDPOINT and run with --ignored"]
async fn modified_time_is_stable_across_stats() {
    let Some(b) = backend() else { return };
    let path = unique_path("stable-mtime");

    b.create(&path, EntryKind::File).await.expect("create");
    b.write(&path, 0, b"content").await.expect("write");

    let first = b.stat(&path).await.expect("stat 1").modified;
    assert!(first.is_some(), "S3 reports a modified time");

    // Wall-clock time passes, but the object itself is never written...
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = b.stat(&path).await.expect("stat 2").modified;

    // ...so the reported mtime must be byte-for-byte identical.
    assert_eq!(
        first, second,
        "mtime must not change while the file is untouched"
    );

    b.remove(&path, EntryKind::File)
        .await
        .expect("cleanup remove");
}

/// `truncate` (a whole-object rewrite) and `rename` (S3 server-side `CopyObject`
/// then delete) against a live bucket. The rename path in particular is only
/// exercised here — the in-memory service can't do a server-side copy and falls
/// back to read+write.
#[tokio::test]
#[ignore = "requires a live S3 endpoint; set RUSTFS_ENDPOINT and run with --ignored"]
async fn truncate_and_server_side_rename() {
    let Some(b) = backend() else { return };
    let src = unique_path("rename-src");
    let dst = unique_path("rename-dst");

    b.create(&src, EntryKind::File).await.expect("create");
    b.write(&src, 0, b"hello world").await.expect("write");

    b.truncate(&src, 5).await.expect("truncate");
    assert_eq!(b.stat(&src).await.expect("stat after truncate").size, 5);
    assert_eq!(
        b.read(&src, 0, 4096).await.expect("read truncated"),
        b"hello"
    );

    b.rename(&src, &dst).await.expect("rename");
    assert!(
        matches!(b.stat(&src).await, Err(StorageError::NotFound)),
        "source is gone after rename"
    );
    assert_eq!(b.read(&dst, 0, 4096).await.expect("read dst"), b"hello");

    b.remove(&dst, EntryKind::File)
        .await
        .expect("cleanup remove");
}
