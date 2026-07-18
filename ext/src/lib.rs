//! FSKit extension logic, in Rust.
//!
//! Defines the `FSUnaryFileSystem` subclass (and, as they land, the `FSVolume`
//! and `FSItem` subclasses) via `objc2`, delegating every operation to a
//! [`StorageBackend`](fskit_s3_core::StorageBackend). Built as a `staticlib` and
//! linked into an Xcode-managed `.appex`; a tiny ObjC stub there calls
//! [`fskit_s3_register`] from `+load` so these Rust-defined classes are
//! registered before FSKit looks up the principal class by name.
//!
//! Status: milestone 1 — bindings + the `FSUnaryFileSystem` subclass compile and
//! register. The volume operations (enumerate/lookup/read) follow.

#![allow(non_snake_case)]

mod sys;

use std::ptr;

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, AllocAnyThread, ClassType};
use objc2_foundation::{NSError, NSString, NSUUID};

use sys::{
    FSContainerIdentifier, FSFileName, FSProbeResult, FSResource, FSTaskOptions, FSUnaryFileSystem,
    FSUnaryFileSystemOperations, FSVolume, FSVolumeIdentifier,
};

define_class!(
    // Our delegate object: a concrete `FSUnaryFileSystem`. FSKit instantiates
    // this (by the name below) as the extension's principal class.
    #[unsafe(super(FSUnaryFileSystem))]
    #[name = "FSKitS3FileSystem"]
    pub struct FileSystem;

    unsafe impl NSObjectProtocol for FileSystem {}

    unsafe impl FSUnaryFileSystemOperations for FileSystem {
        #[unsafe(method(probeResource:replyHandler:))]
        fn probe(
            &self,
            _resource: &FSResource,
            reply: &DynBlock<dyn Fn(*mut FSProbeResult, *mut NSError)>,
        ) {
            // We accept any resource (there's no on-disk format to recognize).
            let container = {
                let uuid = NSUUID::new();
                FSContainerIdentifier::initWithUUID(FSContainerIdentifier::alloc(), &uuid)
            };
            let name = NSString::from_str("fskit-s3");
            let result = FSProbeResult::usable(&name, &container);
            reply.call((Retained::as_ptr(&result) as *mut _, ptr::null_mut()));
        }

        #[unsafe(method(loadResource:options:replyHandler:))]
        fn load(
            &self,
            _resource: &FSResource,
            _options: &FSTaskOptions,
            reply: &DynBlock<dyn Fn(*mut FSVolume, *mut NSError)>,
        ) {
            // Milestone 1: return a bare volume to prove the load path. The real
            // FSVolume subclass (with enumerate/lookup/read) replaces this next.
            let volume = {
                let uuid = NSUUID::new();
                let vid = FSVolumeIdentifier::initWithUUID(FSVolumeIdentifier::alloc(), &uuid);
                let name = FSFileName::nameWithString(&NSString::from_str("fskit-s3"));
                FSVolume::initWithVolumeID_volumeName(FSVolume::alloc(), &vid, &name)
            };
            reply.call((Retained::as_ptr(&volume) as *mut _, ptr::null_mut()));
        }

        #[unsafe(method(unloadResource:options:replyHandler:))]
        fn unload(
            &self,
            _resource: &FSResource,
            _options: &FSTaskOptions,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            reply.call((ptr::null_mut(),));
        }
    }
);

/// Force-register the Rust-defined FSKit classes with the Objective-C runtime.
///
/// Called from the extension bundle's ObjC `+load` stub so the classes exist by
/// the time FSKit resolves the principal class name from `Info.plist`. Idempotent.
#[no_mangle]
pub extern "C" fn fskit_s3_register() {
    let _ = FileSystem::class();
}
