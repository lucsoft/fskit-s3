//! FSKit extension glue — **skeleton, needs full Xcode to build/run**.
//!
//! This crate is where Rust meets FSKit. FSKit is a plain Objective-C framework
//! (verified: `FSUnaryFileSystem.h` etc. ship real ObjC headers, no
//! `.swiftinterface`), so it is driven from Rust with `objc2` exactly the way the
//! sibling `wayland-macos` project drives AppKit — `extern_class!` to reference
//! FSKit's classes, `define_class!` to subclass them, `objc2::rc::Retained` for
//! ownership, `block2` for the reply handlers.
//!
//! ## What FSKit asks of us
//!
//! A "unary" file system (one volume per resource, which is our model — one
//! bucket = one volume) is implemented by:
//!
//! 1. An [`FSUnaryFileSystem`] subclass conforming to `FSUnaryFileSystemOperations`
//!    — `probeResource:` (say whether we recognize a resource) and
//!    `loadResource:options:replyHandler:` (return an `FSVolume` for it).
//! 2. An `FSVolume` subclass conforming to `FSVolumeOperations` (+
//!    `FSVolumePathConfOperations`, which it inherits) for lookup/enumerate/attrs,
//!    and `FSVolumeReadWriteOperations` for `read:`. Each of these maps 1:1 onto a
//!    [`StorageBackend`] call:
//!
//!    | FSKit volume op                        | `StorageBackend` |
//!    |----------------------------------------|------------------|
//!    | `enumerateDirectory:…`                 | `list`           |
//!    | `lookupItemNamed:inDirectory:…`        | `stat`           |
//!    | `getAttributes:ofItem:…`               | `stat`           |
//!    | `readFromFile:…offset:length:…`        | `read`           |
//!
//! 3. `FSItem` subclasses (or one generic item carrying the absolute path +
//!    kind) that we hand back to FSKit and it hands back to us on later calls.
//!
//! The registration is via the app-extension `Info.plist` (`NSExtension` /
//! FSKit's `FSModuleIdentity`) pointing at our principal class — see
//! `../bundle/`. There is no `main()`; `fskitd` instantiates the principal class.
//!
//! ## Why this isn't wired into the workspace build yet
//!
//! The bindings below are the real shape but incomplete, and the crate only
//! yields something *loadable* once assembled into a codesigned `.appex` (full
//! Xcode + a signing identity). The value already delivered and tested lives
//! under it: `fskit-s3-core` (the seam) and `fskit-s3-backend-s3` (S3 + SigV4).
//!
//! The remaining work is mechanical but must be iterated against a running
//! `fskitd`: finish the `extern_class!`/`define_class!` blocks, translate
//! `StorageError` → `FSKitError`/errno, and map `FSItem` identity to paths.

#![allow(dead_code)]

use std::sync::Arc;

use fskit_s3_core::StorageBackend;

/// The backend a mounted volume reads from. Chosen at load time from the
/// resource FSKit hands us (bucket/endpoint/credentials via the mount options or
/// a keychain item); falls back to the in-memory demo backend so the extension
/// can be brought up before S3 config exists.
pub struct VolumeState {
    pub backend: Arc<dyn StorageBackend>,
}

impl VolumeState {
    /// Demo volume: a couple of in-memory objects, no credentials required.
    pub fn demo() -> Self {
        use fskit_s3_core::mem::InMemoryBackend;
        let mut b = InMemoryBackend::new();
        b.insert("readme.txt", b"mounted by fskit-s3\n".to_vec())
            .insert("photos/cover.png", vec![0u8; 32]);
        VolumeState { backend: Arc::new(b) }
    }
}

// ---------------------------------------------------------------------------
// FSKit bindings sketch. Uncomment + complete against the installed FSKit SDK.
// The pattern mirrors wayland-macos/src/mac.rs (define_class!, msg_send, blocks).
// ---------------------------------------------------------------------------
//
// use objc2::rc::Retained;
// use objc2::runtime::NSObject;
// use objc2::{define_class, extern_class, msg_send, ClassType, DefinedClass};
// use objc2_foundation::{NSArray, NSError, NSString};
//
// extern_class!(
//     // #[unsafe(super(NSObject))] — FSUnaryFileSystem : NSObject <FSFileSystemBase>
//     #[unsafe(super(NSObject))]
//     #[name = "FSUnaryFileSystem"]
//     pub struct FSUnaryFileSystem;
// );
//
// define_class!(
//     #[unsafe(super(FSUnaryFileSystem))]
//     #[name = "S3FileSystem"]
//     #[ivars = VolumeState]                 // our Rust state lives on the ObjC instance
//     pub struct S3FileSystem;
//
//     // impl FSUnaryFileSystemOperations:
//     //   - probeResource:replyHandler:   → recognize our resource kind
//     //   - loadResource:options:replyHandler: → build + return the FSVolume
//     unsafe impl S3FileSystem {
//         // #[unsafe(method(...))] wrappers calling into VolumeState/backend.
//     }
// );
//
// Volume ops (FSVolumeOperations + FSVolumeReadWriteOperations) each translate a
// path/FSItem into a backend call and package the result (or a mapped errno)
// into FSKit's reply block. `map_err` below is the single translation point.

/// Translate a backend error into a POSIX errno for FSKit's reply handlers.
pub fn errno_for(err: &fskit_s3_core::StorageError) -> i32 {
    use fskit_s3_core::StorageError::*;
    match err {
        NotFound => libc_enoent(),
        NotADirectory => 20,  // ENOTDIR
        NotAFile => 21,       // EISDIR
        InvalidPath(_) => 22, // EINVAL
        Backend(_) => 5,      // EIO
    }
}

const fn libc_enoent() -> i32 {
    2 // ENOENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_backend_is_mountable_shape() {
        let v = VolumeState::demo();
        let root = v.backend.list("/").unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["photos", "readme.txt"]);
        assert_eq!(v.backend.read("/readme.txt", 0, 4).unwrap(), b"moun");
    }

    #[test]
    fn error_mapping() {
        use fskit_s3_core::StorageError;
        assert_eq!(errno_for(&StorageError::NotFound), 2);
        assert_eq!(errno_for(&StorageError::NotADirectory), 20);
    }
}
