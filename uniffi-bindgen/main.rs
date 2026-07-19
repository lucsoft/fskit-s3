//! The UniFFI binding generator entry point.
//!
//! `uniffi::uniffi_bindgen_main()` is UniFFI's own CLI (`generate`, `scaffolding`,
//! …). We invoke it in *library mode* to emit the Swift bindings for the app's
//! contract, e.g.:
//!
//! ```sh
//! cargo run -p uniffi-bindgen -- generate \
//!   --library target/debug/libfskit_s3_app.dylib \
//!   --language swift --out-dir xcode/host/Generated
//! ```
//!
//! Library mode reads the `#[uniffi::export]` metadata compiled into the app
//! library, so this binary needs no build-time knowledge of the contract itself.
fn main() {
    uniffi::uniffi_bindgen_main()
}
