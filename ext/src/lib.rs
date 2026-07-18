//! FSKit extension logic, in Rust.
//!
//! Defines the `FSUnaryFileSystem`, `FSVolume`, and `FSItem` subclasses via
//! `objc2`, delegating every operation to a
//! [`StorageBackend`](fskit_s3_core::StorageBackend). Built as a `staticlib` and
//! linked into an Xcode-managed `.appex`; a tiny ObjC stub there calls
//! [`fskit_s3_register`] from `+load` so these Rust-defined classes are
//! registered before FSKit looks up the principal class by name.
//!
//! Backend selection is currently the no-credential in-memory demo, so the
//! plumbing can be validated before wiring S3 config + Keychain.

#![allow(non_snake_case)]

mod item;
mod sys;
mod volume;

use std::ptr;
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, AllocAnyThread, ClassType};
use objc2_foundation::{NSError, NSString, NSUUID};

use fskit_s3_core::mem::InMemoryBackend;
use fskit_s3_core::StorageBackend;

use item::S3Item;
use sys::{
    FSContainerIdentifier, FSFileName, FSProbeResult, FSResource, FSTaskOptions, FSUnaryFileSystem,
    FSUnaryFileSystemOperations, FSVolume, FSVolumeIdentifier,
};
use volume::S3Volume;

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
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => {
                    // SAFETY: standard NSError factory with a valid domain + nil userInfo.
                    let error = unsafe {
                        NSError::errorWithDomain_code_userInfo(
                            &NSString::from_str("NSPOSIXErrorDomain"),
                            libc::EIO as isize,
                            None,
                        )
                    };
                    reply.call((ptr::null_mut(), Retained::as_ptr(&error) as *mut NSError));
                    return;
                }
            };
            let volume = {
                let uuid = NSUUID::new();
                let vid = FSVolumeIdentifier::initWithUUID(FSVolumeIdentifier::alloc(), &uuid);
                let name = FSFileName::nameWithString(&NSString::from_str("fskit-s3"));
                S3Volume::new(&vid, &name, demo_backend(), rt)
            };
            reply.call((Retained::as_ptr(&volume) as *mut FSVolume, ptr::null_mut()));
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

/// The backend a mounted volume reads from. For now, a no-credential in-memory
/// demo tree; S3/Keychain configuration replaces this.
fn demo_backend() -> Arc<dyn StorageBackend> {
    let mut b = InMemoryBackend::new();
    b.insert("readme.txt", b"mounted by fskit-s3\n".to_vec())
        .insert("photos/cover.png", vec![0u8; 32]);
    Arc::new(b)
}

/// Force-register the Rust-defined FSKit classes with the Objective-C runtime.
///
/// Called from the extension bundle's ObjC `+load` stub so the classes exist by
/// the time FSKit resolves the principal class name from `Info.plist`. Idempotent.
#[no_mangle]
pub extern "C" fn fskit_s3_register() {
    let _ = FileSystem::class();
    let _ = S3Volume::class();
    let _ = S3Item::class();
}
