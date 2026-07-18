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
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AllocAnyThread, ClassType};
use objc2_foundation::{NSDictionary, NSError, NSString, NSUUID};

use fskit_s3_backend::{OpenDalBackend, S3Config};
use fskit_s3_core::mem::InMemoryBackend;
use fskit_s3_core::StorageBackend;

use item::S3Item;
use sys::{
    FSContainerIdentifier, FSContainerStatus, FSFileName, FSPathURLResource, FSProbeResult,
    FSResource, FSTaskOptions, FSUnaryFileSystem, FSUnaryFileSystemOperations, FSVolume,
    FSVolumeIdentifier,
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
            // The connection's config rides in the source PATH (e.g.
            // `/s3/<name>?bucket=..&region=..&access_key_id=..&endpoint=..`), which
            // FSKit delivers here as an `FSPathURLResource`. Resolving it at LOAD —
            // rather than from `-o` options at `activate` — means a bad config fails
            // the load, which fskitd cleanly unwinds (no stuck instance / "Resource
            // busy"). The secret is never in the path: it comes from the Keychain by
            // `name`, or, when the extension can't read the Keychain (unsigned build),
            // from an `-o secret` that only arrives at `activate` — so a config that's
            // valid but still lacks a secret is deferred, not failed.
            let source = source_path(_resource);
            log_line(&format!("loadResource source = {source:?}"));
            let outcome = match build_backend(parse_source_path(&source)) {
                Ok(outcome) => outcome,
                Err(msg) => {
                    log_line(&format!("loadResource failed: {msg}"));
                    // Signal the failed load so fskitd tears the instance down.
                    self.set_container_state(ContainerState::NotReady);
                    // Carry the reason so `mount` prints it, not "Invalid argument".
                    let err = error_with_message(libc::EINVAL, &msg);
                    reply.call((ptr::null_mut(), Retained::as_ptr(&err) as *mut NSError));
                    return;
                }
            };
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
            match outcome {
                BuildOutcome::Ready(backend) => volume.set_backend(backend),
                BuildOutcome::NeedSecret(pending) => {
                    log_line("loadResource: config valid, secret deferred to activate");
                    volume.set_pending(pending);
                }
            }
            // Transition the container to `ready`; without this FSKit rejects the
            // load with "unexpected container state" (POSIX 35).
            self.set_container_state(ContainerState::Ready);
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

/// Return the loaded container to `notReady`, from anywhere in the extension,
/// carrying the failure as the container's status when known.
///
/// `loadResource` marks the container `ready` before the mount's `-o` options are
/// available, so the backend is only chosen later in the volume's `activate`. When
/// that fails (bad config / no secret), signal the container back to `notReady`
/// so fskitd can tear the loaded resource down instead of leaving the instance
/// stuck (which makes the next mount fail at probe with "Resource busy").
///
/// `status` is the reason (the same `NSError` the op replies with) — passed to
/// `notReadyWithStatus:` so fskitd/logs get the actual failure, not a bare
/// not-ready.
pub fn signal_container_not_ready(status: Option<&NSError>) {
    if let Some(&ptr) = FILESYSTEM.get() {
        // SAFETY: the cached pointer is our leaked, process-lifetime FileSystem
        // singleton (set in `fskit_s3_make_filesystem`).
        let fs = unsafe { &*(ptr as *const FileSystem) };
        fs.setContainerStatus(&FSContainerStatus::notReadyWithStatus(status));
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

/// The local path of a `mount` resource (our config-carrying source path), or
/// empty when it isn't a path-URL resource.
fn source_path(resource: &FSResource) -> String {
    resource
        .downcast_ref::<FSPathURLResource>()
        .and_then(|r| r.url().path())
        .map(|p| p.to_string())
        .unwrap_or_default()
}

/// Parse a source path into the same `key=value` option map the config uses.
///
/// The path is `/<type>[/<name>][?k=v&k=v…]` — e.g. `/memory` or
/// `/s3/<name>?bucket=..&region=..&access_key_id=..&endpoint=..`. The first
/// segment is `type`, the second (if any) `name`, and the query carries the rest.
/// Values may not contain `&`/`=` (the app validates this); the secret is never here.
fn parse_source_path(path: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let trimmed = path.trim_start_matches('/');
    let (head, query) = trimmed.split_once('?').unwrap_or((trimmed, ""));
    let mut segs = head.splitn(2, '/');
    if let Some(t) = segs.next().filter(|s| !s.is_empty()) {
        map.insert("type".to_string(), t.to_string());
    }
    if let Some(name) = segs.next().filter(|s| !s.is_empty()) {
        map.insert("name".to_string(), name.to_string());
    }
    for part in query.split('&') {
        if let Some((key, value)) = part.split_once('=') {
            map.insert(key.trim().to_string(), value.to_string());
        }
    }
    map
}

/// The result of resolving a config map into a backend.
pub(crate) enum BuildOutcome {
    /// A ready-to-serve backend.
    Ready(Arc<dyn StorageBackend>),
    /// The config is valid but no secret was available yet (the extension couldn't
    /// read the Keychain, so it must come from an `-o secret` at `activate`). Carries
    /// the parsed config so `activate` can finish once the secret arrives.
    NeedSecret(HashMap<String, String>),
}

/// Resolve a parsed config map into a backend, dispatching on `type`.
///
/// `type=memory` ⇒ the demo; `type=s3` ⇒ an S3 bucket. A missing/unknown `type` or
/// missing required S3 fields is an `Err` — the caller fails the **load**, which
/// fskitd cleanly unwinds. A valid S3 config with no obtainable secret is
/// `Ok(NeedSecret)` — deferred to `activate`, not failed.
pub(crate) fn build_backend(opts: HashMap<String, String>) -> Result<BuildOutcome, String> {
    match opts.get("type").map(String::as_str) {
        Some("memory") => Ok(BuildOutcome::Ready(demo_backend())),
        Some("s3") => build_s3_backend(opts),
        Some(other) => Err(format!("unknown connection type {other:?}")),
        None => Err("no connection type in source path".to_string()),
    }
}

/// Build the S3 backend from a parsed config map, or defer if only the secret is
/// missing. Takes ownership so it can hand the map back in [`BuildOutcome::NeedSecret`].
fn build_s3_backend(opts: HashMap<String, String>) -> Result<BuildOutcome, String> {
    let name = opts.get("name").cloned().unwrap_or_default();
    let bucket = opts.get("bucket").cloned().unwrap_or_default();
    let access_key_id = opts.get("access_key_id").cloned().unwrap_or_default();
    if bucket.is_empty() || access_key_id.is_empty() {
        return Err(format!(
            "S3 connection {name:?}: missing bucket/access_key_id"
        ));
    }
    // Secret: Keychain by name (the secure default), else an `-o secret` — which is
    // only present once this map has been merged with activate's options.
    let Some(secret_access_key) =
        read_keychain_secret(&name).or_else(|| opts.get("secret").cloned())
    else {
        return Ok(BuildOutcome::NeedSecret(opts));
    };
    let cfg = S3Config {
        bucket,
        region: opts.get("region").cloned().unwrap_or_default(),
        endpoint: opts.get("endpoint").cloned().unwrap_or_default(),
        access_key_id,
        secret_access_key,
        session_token: opts.get("session_token").cloned(),
    };
    let backend = OpenDalBackend::s3(&cfg).map_err(|e| e.to_string())?;
    Ok(BuildOutcome::Ready(Arc::new(backend)))
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

/// An `NSError` whose `localizedDescription` is `message`, so `mount` prints the
/// real reason ("… missing bucket/access_key_id") instead of the generic errno
/// text ("Invalid argument"). `mount` shows an NSError's `localizedDescription`,
/// which honours an explicit `NSLocalizedDescription` in `userInfo`.
pub(crate) fn error_with_message(errno: i32, message: &str) -> Retained<NSError> {
    let key = NSString::from_str("NSLocalizedDescription");
    let value = NSString::from_str(message);
    // userInfo is `NSDictionary<NSString, AnyObject>`, so the value goes in as an
    // untyped object.
    let value_obj: &AnyObject = &value;
    let user_info = NSDictionary::from_slices(&[&*key], &[value_obj]);
    // SAFETY: standard NSError factory; domain string + a valid userInfo dict.
    unsafe {
        NSError::errorWithDomain_code_userInfo(
            &NSString::from_str("NSPOSIXErrorDomain"),
            errno as isize,
            Some(&user_info),
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
