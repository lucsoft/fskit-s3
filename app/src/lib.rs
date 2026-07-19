//! `fskit-s3-app` — the logic + Rust↔Swift contract behind the fskit-s3 macOS app.
//!
//! The UI is native **SwiftUI** (`xcode/host/*.swift`); this crate owns everything
//! the app *does* and exposes it to Swift through a UniFFI contract ([`ffi`]):
//!
//! - [`connection`] — the `Connection`/`ConnectionKind` (`Memory` / `S3`) model +
//!   the persisted `Registry` (`connections.json`, never holding a secret).
//! - [`keychain`] — the S3 secret in the macOS Keychain (secure path).
//! - [`disksecret`] — a dev-only **plaintext** secret file, for unsigned builds
//!   where the extension can't read the shared Keychain group (insecure, opt-in).
//! - [`s3check`] — the "Test and Save" credential check (lists the bucket).
//! - [`mounts`] — the mount table + `mount`/`unmount`. No bespoke CLI: mounting is
//!   the system `mount` tool with the connection's config.
//! - [`health`] — the FSKit extension-health query (`FSClient`, via objc2).
//! - [`autostart`] — launch-at-login (`SMAppService`, via objc2).
//! - [`ffi`] — the `#[uniffi::export]` surface the SwiftUI app calls.
//!
//! Everything here is dependency-light Rust (plus the small objc2/FSClient/
//! SMAppService FFI in `health`/`autostart`). The AppKit UI that used to live here
//! is gone — it moved to SwiftUI over the contract.

// The app must not panic in normal operation: no unwrap/expect/panic/indexing
// outside tests. Enforced by clippy in CI.
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

mod autostart;
mod connection;
mod disksecret;
mod ffi;
mod health;
mod keychain;
mod mounts;
mod s3check;

// The UniFFI scaffolding for the Rust↔Swift contract declared in `ffi.rs`. This
// macro emits the FFI shims + the metadata `uniffi-bindgen` reads to generate the
// Swift bindings. Proc-macro (UDL-less) mode, so no `.udl` and no build.rs.
uniffi::setup_scaffolding!();
