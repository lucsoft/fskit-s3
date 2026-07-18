//! FSKit extension logic, in Rust.
//!
//! Defines the `FSUnaryFileSystem`, `FSVolume`, and `FSItem` subclasses via
//! `objc2`, delegating every operation to a
//! [`StorageBackend`](fskit_s3_core::StorageBackend). Built as a `staticlib` and
//! linked into an Xcode-managed `.appex`; the Swift `@main` bootstrap calls
//! [`fskit_s3_make_filesystem`], which registers these Rust-defined classes and
//! returns the (cached, singleton) delegate instance.
//!
//! `loadResource` picks the backend from the mount's `-o` options (see
//! [`backend_for`]), dispatching on an explicit `type`: `type=s3` ⇒ an S3 bucket
//! (secret from the shared Keychain access group, else an `-o secret`),
//! `type=memory` ⇒ the in-memory demo. A missing `type` fails the mount — the
//! extension refuses to guess rather than silently serving the demo.

#![allow(non_snake_case)]

mod item;
mod oslog;
mod sys;
mod volume;

use std::collections::HashMap;
use std::ptr;
use std::sync::{Arc, OnceLock};

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, AllocAnyThread, ClassType};
use objc2_foundation::{NSError, NSString, NSUUID};

use fskit_s3_backend::{OpenDalBackend, S3Config};
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
            // NB: `loadResource` does NOT receive the mount's `-o` options — FSKit
            // delivers those to the volume's `activateWithOptions:` (they are parsed
            // per `FSActivateOptionSyntax`). So the backend can't be chosen here; we
            // build the volume now (with its tokio runtime) and defer backend
            // selection to `activate`, which is where the config actually arrives.
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => {
                    let err = posix_error(libc::EIO);
                    reply.call((ptr::null_mut(), Retained::as_ptr(&err) as *mut NSError));
                    return;
                }
            };
            let volume = {
                let uuid = NSUUID::new();
                let vid = FSVolumeIdentifier::initWithUUID(FSVolumeIdentifier::alloc(), &uuid);
                let name = FSFileName::nameWithString(&NSString::from_str("fskit-s3"));
                S3Volume::new(&vid, &name, rt)
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

/// The no-credential in-memory demo tree (served only for the explicit `memory`
/// connection).
fn demo_backend() -> Arc<dyn StorageBackend> {
    let mut b = InMemoryBackend::new();
    b.insert("readme.txt", b"mounted by fskit-s3\n".to_vec())
        .insert("photos/cover.png", vec![0u8; 32]);
    Arc::new(b)
}

/// Keychain identity shared with the app (`app/src/keychain.rs`).
const KEYCHAIN_SERVICE: &str = "dev.lucsoft.fskit-s3";
const KEYCHAIN_ACCESS_GROUP: &str = "H8563U643B.dev.lucsoft.fskit-s3";

/// Choose the backend for a mount from its `-o` options, dispatching on the
/// explicit `type` the app always sends.
///
/// `type=memory` ⇒ the demo; `type=s3` ⇒ an S3 bucket (secret from `Keychain[name]`
/// else the `-o secret`). A **missing `type`** is an error — we refuse to guess and
/// never silently fall back to the demo, so a config/`-o`-delivery problem fails
/// the mount loudly instead of masquerading as the demo.
fn backend_for(options: &FSTaskOptions) -> Result<Arc<dyn StorageBackend>, String> {
    let raw = raw_task_options(options);
    log_line(&format!(
        "loadResource taskOptions ({}): {raw:?}",
        raw.len()
    ));
    let opts = parse_options(&raw);

    match opts.get("type").map(String::as_str) {
        Some("memory") => Ok(demo_backend()),
        Some("s3") => build_s3_backend(&opts),
        Some(other) => Err(format!("unknown connection type {other:?}")),
        None => Err(format!(
            "no connection type in mount options — refusing to guess (taskOptions: {raw:?})"
        )),
    }
}

/// Build the S3 backend from the parsed `-o` options.
fn build_s3_backend(opts: &HashMap<String, String>) -> Result<Arc<dyn StorageBackend>, String> {
    let name = opts.get("name").map(String::as_str).unwrap_or("");
    let bucket = opts.get("bucket").cloned().unwrap_or_default();
    let access_key_id = opts.get("access_key_id").cloned().unwrap_or_default();
    if bucket.is_empty() || access_key_id.is_empty() {
        return Err(format!(
            "S3 connection {name:?}: missing bucket/access_key_id"
        ));
    }
    let secret_access_key = read_keychain_secret(name)
        .or_else(|| opts.get("secret").cloned())
        .ok_or_else(|| format!("S3 connection {name:?}: no secret (Keychain or -o secret)"))?;

    let cfg = S3Config {
        bucket,
        region: opts.get("region").cloned().unwrap_or_default(),
        endpoint: opts.get("endpoint").cloned().unwrap_or_default(),
        access_key_id,
        secret_access_key,
        session_token: opts.get("session_token").cloned(),
    };
    let backend = OpenDalBackend::s3(&cfg).map_err(|e| e.to_string())?;
    Ok(Arc::new(backend))
}

/// The raw `FSTaskOptions.taskOptions` tokens (the argv-equivalent array).
fn raw_task_options(options: &FSTaskOptions) -> Vec<String> {
    let tokens = options.taskOptions();
    (0..tokens.count())
        .map(|i| tokens.objectAtIndex(i).to_string())
        .collect()
}

/// Parse `key=value` pairs out of the raw task-option tokens. Each token may be a
/// single `key=value`, a bare flag like `-o`, or the whole comma-joined `-o`
/// string — so split on commas first and keep only the `key=value` parts.
fn parse_options(raw: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for token in raw {
        for part in token.split(',') {
            if let Some((key, value)) = part.split_once('=') {
                map.insert(key.trim().to_string(), value.to_string());
            }
        }
    }
    map
}

/// Read an S3 connection's secret from the shared Keychain access group (the item
/// the app stored). `None` if absent or unreadable.
fn read_keychain_secret(name: &str) -> Option<String> {
    use security_framework::passwords::{generic_password, PasswordOptions};
    let mut opts = PasswordOptions::new_generic_password(KEYCHAIN_SERVICE, name);
    opts.set_access_group(KEYCHAIN_ACCESS_GROUP);
    let bytes = generic_password(opts).ok()?;
    String::from_utf8(bytes).ok()
}

/// Build a `Retained<NSError>` for a POSIX errno (nil userInfo). The caller keeps
/// it alive across the reply block (the pointer is borrowed, +0).
fn posix_error(errno: i32) -> Retained<NSError> {
    // SAFETY: standard NSError factory with a valid domain string + nil userInfo.
    unsafe {
        NSError::errorWithDomain_code_userInfo(
            &NSString::from_str("NSPOSIXErrorDomain"),
            errno as isize,
            None,
        )
    }
}

/// Log a line to the unified log (visible via `log stream` / the app's dev-mode
/// tail), prefixed `[fskit-s3]`. The extension is headless, so this is how
/// mount-time decisions and failures surface for debugging.
///
/// Routed through [`oslog::log_public`] so it's emitted as PUBLIC text: `NSLog`
/// stores its message as a redacted argument (shows as `<private>` unless the
/// machine has private-data logging on), which hid these lines exactly when they
/// were needed.
fn log_line(message: &str) {
    oslog::log_public(&format!("[fskit-s3] {message}"));
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
