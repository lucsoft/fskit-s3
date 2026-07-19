//! `fskit-s3-app` binary entry point.
//!
//! All logic lives in the library crate ([`fskit_s3_app`]); this is just the
//! standalone `cargo run` entry. The Xcode `fskit-s3-host` target instead calls
//! the C-ABI `fskit_s3_app_run` export (see `lib.rs`) from its Swift bootstrap,
//! so the same Rust app powers both the dev binary and the shipped host that
//! carries the FSKit extension.

fn main() {
    fskit_s3_app::run();
}
