//! FSKit extension logic, in Rust.
//!
//! Defines the `FSUnaryFileSystem`, `FSVolume`, and `FSItem` subclasses via
//! `objc2`, delegating every operation to a
//! [`StorageBackend`](fskit_s3_core::StorageBackend). Built as a `staticlib` and
//! linked into an Xcode-managed `.appex`; the Swift `@main` bootstrap calls
//! [`fskit_s3_make_filesystem`], which registers these Rust-defined classes and
//! returns the (cached, singleton) delegate instance.
//!
//! Backend selection is currently the no-credential in-memory demo, so the
//! plumbing can be validated before wiring S3 config + Keychain.

#![allow(non_snake_case)]

mod item;
mod sys;
mod volume;

use std::ptr;
use std::sync::{Arc, OnceLock};

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, AllocAnyThread, ClassType};
use objc2_foundation::{NSError, NSString, NSUUID};

use fskit_s3_core::mem::InMemoryBackend;
use fskit_s3_core::StorageBackend;

use item::S3Item;
use sys::{
    FSContainerIdentifier, FSContainerStatus, FSFileName, FSProbeResult, FSResource, FSTaskOptions,
    FSUnaryFileSystem, FSUnaryFileSystemOperations, FSVolume, FSVolumeIdentifier,
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
            // The container identity MUST be stable across probe calls (FSKit
            // probes the same resource more than once) — a fresh UUID each time
            // yields two containers for one resource ("unexpected container state").
            let container = {
                let uuid = container_uuid();
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
            // Transition the container from `notReady` to `ready`; without this
            // FSKit rejects the load with "unexpected container state" (POSIX 35).
            self.set_container_state(ContainerState::Ready);
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
            // Transfer ownership (+1) to FSKit: it holds the volume for the whole
            // mount, well past this call, so a borrowed pointer would dangle.
            let volume_ptr = Retained::into_raw(volume) as *mut FSVolume;
            reply.call((volume_ptr, ptr::null_mut()));
        }

        #[unsafe(method(unloadResource:options:replyHandler:))]
        fn unload(
            &self,
            _resource: &FSResource,
            _options: &FSTaskOptions,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            // Return the container to `notReady` so a later mount starts clean;
            // otherwise fskitd keeps it "ready" and the next mount conflicts.
            self.set_container_state(ContainerState::NotReady);
            reply.call((ptr::null_mut(),));
        }
    }
);

/// The FSKit container lifecycle state (mirrors `FSContainerState`).
///
/// FSKit *drives* the transitions by calling our delegate methods in an order it
/// chooses, so this is a value-typed enum reported via `containerStatus` — not a
/// consuming `self -> Container<Next>` typestate (the delegate is a fixed-class
/// ObjC object FSKit holds, so it can't change type). Modeling the states as an
/// enum still keeps the value type-checked and the transitions readable.
#[derive(Clone, Copy)]
#[allow(dead_code)] // `Active` is set by FSKit, not us — kept for completeness.
enum ContainerState {
    NotReady,
    Ready,
    Active,
}

impl FileSystem {
    fn set_container_state(&self, state: ContainerState) {
        let status = match state {
            ContainerState::NotReady => FSContainerStatus::notReadyWithStatus(None),
            ContainerState::Ready => FSContainerStatus::ready(),
            ContainerState::Active => FSContainerStatus::active(),
        };
        self.setContainerStatus(&status);
    }
}

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

/// A process-stable container UUID (raw pointer as usize for `Send`/`Sync`), so
/// every probe reports the same container identity for our synthetic container.
static CONTAINER_UUID: OnceLock<usize> = OnceLock::new();

fn container_uuid() -> Retained<NSUUID> {
    let ptr = *CONTAINER_UUID.get_or_init(|| Retained::into_raw(NSUUID::new()) as usize);
    // SAFETY: the cached pointer is a leaked, never-freed NSUUID.
    unsafe { Retained::retain(ptr as *mut NSUUID) }.expect("cached NSUUID is live")
}

/// The single file-system delegate, stored as a leaked raw pointer (usize so the
/// `OnceLock` is `Send`/`Sync`). It lives for the whole process.
static FILESYSTEM: OnceLock<usize> = OnceLock::new();

/// Construct (and cache) the extension's file-system delegate.
///
/// The Swift `@main UnaryFileSystemExtension` bootstrap (the only Swift in the
/// project) calls this and returns the result as its `fileSystem` — so the
/// principal object is our Rust-defined class while ExtensionKit's entry point
/// stays minimal.
#[no_mangle]
pub extern "C" fn fskit_s3_make_filesystem() -> *mut FSUnaryFileSystem {
    fskit_s3_register();
    // MUST be a singleton: the Swift @main's `fileSystem` property is read
    // repeatedly by FSKit, and a fresh instance per read registers a duplicate
    // container ("resource already exists" / "unexpected container state").
    let ptr = *FILESYSTEM.get_or_init(|| {
        let fs: Retained<FileSystem> = unsafe { msg_send![FileSystem::alloc(), init] };
        Retained::into_raw(fs) as usize
    });
    ptr as *mut FSUnaryFileSystem
}
