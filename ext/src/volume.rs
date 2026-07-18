//! `FSVolume` subclass: maps FSKit's volume operations onto a `StorageBackend`.
//!
//! Read-only. The read path (activate/lookup/getAttributes/enumerate/read) is
//! implemented against the backend; every mutating operation replies `EROFS`.
//! Each FSKit call runs the backend future to completion on the volume's tokio
//! runtime and fires the reply block with the result (or a POSIX error).

use std::ptr;
use std::sync::{Arc, OnceLock};

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSData, NSError, NSString};
use tokio::runtime::Runtime;

use fskit_s3_core::{path as corepath, StorageBackend, StorageError};

use crate::item::{item_id_for, S3Item};
use crate::sys::*;

/// Volume state carried on the ObjC instance.
///
/// The backend is chosen from the mount's `-o` options, which FSKit delivers to
/// `activateWithOptions:` (not `loadResource:`) — so it's filled in at activate
/// time, once, via a `OnceLock`. Every operation runs after activate, so the
/// backend is set by the time it's read; a still-empty lock is treated as EIO.
pub struct VolumeIvars {
    backend: OnceLock<Arc<dyn StorageBackend>>,
    rt: Runtime,
}

define_class!(
    #[unsafe(super(FSVolume))]
    #[name = "FSKitS3Volume"]
    #[ivars = VolumeIvars]
    pub struct S3Volume;

    unsafe impl NSObjectProtocol for S3Volume {}

    unsafe impl FSVolumePathConfOperations for S3Volume {
        #[unsafe(method(maximumLinkCount))]
        fn maximumLinkCount(&self) -> isize {
            1
        }
        #[unsafe(method(maximumNameLength))]
        fn maximumNameLength(&self) -> isize {
            255
        }
        #[unsafe(method(restrictsOwnershipChanges))]
        fn restrictsOwnershipChanges(&self) -> bool {
            true
        }
        #[unsafe(method(truncatesLongNames))]
        fn truncatesLongNames(&self) -> bool {
            false
        }
    }

    unsafe impl FSVolumeOperations for S3Volume {
        #[unsafe(method(supportedVolumeCapabilities))]
        fn supportedVolumeCapabilities(&self) -> *mut FSVolumeSupportedCapabilities {
            // Property getter: hand back an autoreleased (+0) object.
            Retained::autorelease_return(FSVolumeSupportedCapabilities::new())
        }

        #[unsafe(method(volumeStatistics))]
        fn volumeStatistics(&self) -> *mut FSStatFSResult {
            let r = FSStatFSResult::initWithFileSystemTypeName(
                FSStatFSResult::alloc(),
                &NSString::from_str("fskit-s3"),
            );
            r.setBlockSize(4096);
            r.setTotalBlocks(1 << 20);
            r.setUsedBlocks(1 << 20);
            r.setFreeBlocks(0);
            r.setAvailableBlocks(0);
            Retained::autorelease_return(r)
        }

        #[unsafe(method(mountWithOptions:replyHandler:))]
        fn mount(&self, _options: &FSTaskOptions, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(unmountWithReplyHandler:))]
        fn unmount(&self, reply: &DynBlock<dyn Fn()>) {
            reply.call(());
        }

        #[unsafe(method(synchronizeWithFlags:replyHandler:))]
        fn synchronize(&self, _flags: FSSyncFlags, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(activateWithOptions:replyHandler:))]
        fn activate(
            &self,
            options: &FSTaskOptions,
            reply: &DynBlock<dyn Fn(*mut FSItem, *mut NSError)>,
        ) {
            // Log which build is actually serving this mount. This is the only
            // signal that reveals daemon-cache staleness (right bundle on disk,
            // stale loaded process): the host compares the on-disk Info.plist SHA,
            // but only the running process can report its own compiled-in SHA.
            crate::log_line(&format!("activate: build {}", env!("FSKIT_S3_GIT_SHA")));
            // The mount's `-o` config arrives HERE (not at loadResource), so this
            // is where the backend is chosen. A misconfigured connection fails the
            // activation (EINVAL) rather than mounting an unusable volume.
            match crate::backend_for(options) {
                Ok(backend) => {
                    // Set once; activate runs a single time per mount.
                    let _ = self.ivars().backend.set(backend);
                }
                Err(msg) => {
                    crate::log_line(&format!("activate failed: {msg}"));
                    reply.call((
                        ptr::null_mut(),
                        Retained::as_ptr(&err(libc::EINVAL)) as *mut NSError,
                    ));
                    return;
                }
            }
            // FSKit holds the root item for the mount's lifetime (until
            // reclaimItem), so transfer ownership (+1) rather than lend it.
            let root = S3Item::new("/".to_string(), true, 0);
            let root_fsitem: *mut FSItem = Retained::into_raw(root) as *mut FSItem;
            reply.call((root_fsitem, ptr::null_mut()));
        }

        #[unsafe(method(deactivateWithOptions:replyHandler:))]
        fn deactivate(
            &self,
            _options: FSDeactivateOptions,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(getAttributes:ofItem:replyHandler:))]
        fn getAttributes(
            &self,
            _desired: &FSItemGetAttributesRequest,
            item: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        ) {
            let Some(item) = item.downcast_ref::<S3Item>() else {
                reply.call((
                    ptr::null_mut(),
                    Retained::as_ptr(&err(libc::EIO)) as *mut NSError,
                ));
                return;
            };
            let attrs = attributes_for(item);
            reply.call((
                Retained::as_ptr(&attrs) as *mut FSItemAttributes,
                ptr::null_mut(),
            ));
        }

        #[unsafe(method(lookupItemNamed:inDirectory:replyHandler:))]
        fn lookup(
            &self,
            name: &FSFileName,
            directory: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            let (Some(dir), Some(name_str)) =
                (directory.downcast_ref::<S3Item>(), file_name_string(name))
            else {
                reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::as_ptr(&err(libc::EIO)) as *mut NSError,
                ));
                return;
            };
            let Some(backend) = self.backend() else {
                reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::as_ptr(&err(libc::EIO)) as *mut NSError,
                ));
                return;
            };
            let child = join(dir.path(), &name_str);
            match self.ivars().rt.block_on(backend.stat(&child)) {
                Ok(entry) => {
                    // FSKit keeps the item (until reclaimItem) → transfer ownership.
                    // The name is copied synchronously, so lending it is fine.
                    let item = S3Item::new(child, entry.is_dir(), entry.size);
                    let fname = FSFileName::nameWithString(&NSString::from_str(&name_str));
                    reply.call((
                        Retained::into_raw(item) as *mut FSItem,
                        Retained::as_ptr(&fname) as *mut FSFileName,
                        ptr::null_mut(),
                    ));
                }
                Err(e) => reply.call((
                    ptr::null_mut(),
                    ptr::null_mut(),
                    Retained::as_ptr(&err(errno(&e))) as *mut NSError,
                )),
            }
        }

        #[unsafe(method(reclaimItem:replyHandler:))]
        fn reclaim(&self, _item: &FSItem, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            // Nothing to free: each S3Item is a plain retained object.
            reply.call((ptr::null_mut(),));
        }

        #[unsafe(method(enumerateDirectory:startingAtCookie:verifier:providingAttributes:usingPacker:replyHandler:))]
        fn enumerate(
            &self,
            directory: &FSItem,
            cookie: FSDirectoryCookie,
            verifier: FSDirectoryVerifier,
            _attributes: *mut FSItemGetAttributesRequest,
            packer: &FSDirectoryEntryPacker,
            reply: &DynBlock<dyn Fn(FSDirectoryVerifier, *mut NSError)>,
        ) {
            let Some(dir) = directory.downcast_ref::<S3Item>() else {
                reply.call((verifier, Retained::as_ptr(&err(libc::EIO)) as *mut NSError));
                return;
            };
            let Some(backend) = self.backend() else {
                reply.call((verifier, Retained::as_ptr(&err(libc::EIO)) as *mut NSError));
                return;
            };
            let entries = match self.ivars().rt.block_on(backend.list(dir.path())) {
                Ok(entries) => entries,
                Err(e) => {
                    reply.call((verifier, Retained::as_ptr(&err(errno(&e))) as *mut NSError));
                    return;
                }
            };
            // Resume from `cookie` (the next-cookie we handed out last time).
            let parent_id = item_id_for(dir.path());
            for (i, entry) in entries.iter().enumerate().skip(cookie as usize) {
                let fname = FSFileName::nameWithString(&NSString::from_str(&entry.name));
                let item_type = if entry.is_dir() {
                    FS_ITEM_TYPE_DIRECTORY
                } else {
                    FS_ITEM_TYPE_FILE
                };
                let id = item_id_for(&join(dir.path(), &entry.name));
                let next_cookie = (i + 1) as FSDirectoryCookie;
                // Pack the attributes inline — FSKit drops entries that lack them.
                let attrs = FSItemAttributes::new();
                attrs.setType(item_type);
                attrs.setMode(if entry.is_dir() { 0o40755 } else { 0o100644 });
                attrs.setLinkCount(1);
                attrs.setUid(0);
                attrs.setGid(0);
                attrs.setSize(entry.size);
                attrs.setAllocSize(entry.size);
                attrs.setFileID(id);
                attrs.setParentID(parent_id);
                let packed = packer.packEntry(&fname, item_type, id, next_cookie, Some(&attrs));
                if !packed {
                    break; // buffer full; FSKit will call again with this cookie
                }
            }
            reply.call((verifier, ptr::null_mut()));
        }

        // ---- mutating operations: read-only volume ----
        #[unsafe(method(setAttributes:onItem:replyHandler:))]
        fn setAttributes(
            &self,
            _attrs: &FSItemSetAttributesRequest,
            _item: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                Retained::as_ptr(&err(libc::EROFS)) as *mut NSError,
            ));
        }

        #[unsafe(method(readSymbolicLink:replyHandler:))]
        fn readSymbolicLink(
            &self,
            _item: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                Retained::as_ptr(&err(libc::EINVAL)) as *mut NSError,
            ));
        }

        #[unsafe(method(createItemNamed:type:inDirectory:attributes:replyHandler:))]
        fn createItem(
            &self,
            _name: &FSFileName,
            _item_type: FSItemType,
            _directory: &FSItem,
            _attributes: &FSItemSetAttributesRequest,
            reply: &DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                ptr::null_mut(),
                Retained::as_ptr(&err(libc::EROFS)) as *mut NSError,
            ));
        }

        #[unsafe(method(createSymbolicLinkNamed:inDirectory:attributes:linkContents:replyHandler:))]
        fn createSymbolicLink(
            &self,
            _name: &FSFileName,
            _directory: &FSItem,
            _attributes: &FSItemSetAttributesRequest,
            _contents: &FSFileName,
            reply: &DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                ptr::null_mut(),
                Retained::as_ptr(&err(libc::EROFS)) as *mut NSError,
            ));
        }

        #[unsafe(method(createLinkToItem:named:inDirectory:replyHandler:))]
        fn createLink(
            &self,
            _item: &FSItem,
            _name: &FSFileName,
            _directory: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                Retained::as_ptr(&err(libc::EROFS)) as *mut NSError,
            ));
        }

        #[unsafe(method(removeItem:named:fromDirectory:replyHandler:))]
        fn removeItem(
            &self,
            _item: &FSItem,
            _name: &FSFileName,
            _directory: &FSItem,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            reply.call((Retained::as_ptr(&err(libc::EROFS)) as *mut NSError,));
        }

        #[unsafe(method(renameItem:inDirectory:named:toNewName:inDirectory:overItem:replyHandler:))]
        #[allow(clippy::too_many_arguments)]
        fn renameItem(
            &self,
            _item: &FSItem,
            _source_directory: &FSItem,
            _source_name: &FSFileName,
            _destination_name: &FSFileName,
            _destination_directory: &FSItem,
            _over_item: *mut FSItem,
            reply: &DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        ) {
            reply.call((
                ptr::null_mut(),
                Retained::as_ptr(&err(libc::EROFS)) as *mut NSError,
            ));
        }
    }

    unsafe impl FSVolumeReadWriteOperations for S3Volume {
        #[unsafe(method(readFromFile:offset:length:intoBuffer:replyHandler:))]
        fn read(
            &self,
            item: &FSItem,
            offset: i64,
            length: usize,
            buffer: &FSMutableFileDataBuffer,
            reply: &DynBlock<dyn Fn(usize, *mut NSError)>,
        ) {
            let Some(item) = item.downcast_ref::<S3Item>() else {
                reply.call((0, Retained::as_ptr(&err(libc::EIO)) as *mut NSError));
                return;
            };
            let Some(backend) = self.backend() else {
                reply.call((0, Retained::as_ptr(&err(libc::EIO)) as *mut NSError));
                return;
            };
            let cap = length.min(buffer.length());
            let data =
                match self
                    .ivars()
                    .rt
                    .block_on(backend.read(item.path(), offset.max(0) as u64, cap))
                {
                    Ok(data) => data,
                    Err(e) => {
                        reply.call((0, Retained::as_ptr(&err(errno(&e))) as *mut NSError));
                        return;
                    }
                };
            let n = data.len().min(cap);
            let dst = buffer.mutableBytes() as *mut u8;
            if !dst.is_null() && n > 0 {
                // SAFETY: `dst` is FSKit's buffer of at least `buffer.length()`
                // bytes; `n <= cap <= buffer.length()`, and `data` holds `n` bytes.
                unsafe { ptr::copy_nonoverlapping(data.as_ptr(), dst, n) };
            }
            reply.call((n, ptr::null_mut()));
        }

        #[unsafe(method(writeContents:toFile:atOffset:replyHandler:))]
        fn write(
            &self,
            _contents: &NSData,
            _item: &FSItem,
            _offset: i64,
            reply: &DynBlock<dyn Fn(usize, *mut NSError)>,
        ) {
            reply.call((0, Retained::as_ptr(&err(libc::EROFS)) as *mut NSError));
        }
    }
);

impl S3Volume {
    /// Build a volume whose futures run on `rt`. The backend is not known yet —
    /// it's chosen from the `-o` options at `activate` time and stored then.
    pub fn new(volume_id: &FSVolumeIdentifier, name: &FSFileName, rt: Runtime) -> Retained<Self> {
        let this = Self::alloc().set_ivars(VolumeIvars {
            backend: OnceLock::new(),
            rt,
        });
        unsafe { msg_send![super(this), initWithVolumeID: volume_id, volumeName: name] }
    }

    /// The backend selected at activate, or `None` if activate hasn't set it
    /// (or failed to build one). Callers map `None` to EIO.
    fn backend(&self) -> Option<&Arc<dyn StorageBackend>> {
        self.ivars().backend.get()
    }
}

/// Build an `FSItemAttributes` snapshot for an item.
fn attributes_for(item: &S3Item) -> Retained<FSItemAttributes> {
    let attrs = FSItemAttributes::new();
    if item.is_dir() {
        attrs.setType(FS_ITEM_TYPE_DIRECTORY);
        attrs.setMode(0o40755);
    } else {
        attrs.setType(FS_ITEM_TYPE_FILE);
        attrs.setMode(0o100644);
    }
    attrs.setLinkCount(1);
    attrs.setUid(0);
    attrs.setGid(0);
    attrs.setSize(item.size());
    attrs.setAllocSize(item.size());
    attrs.setFileID(item.item_id());
    attrs.setParentID(item_id_for(corepath::parent(item.path())));
    attrs
}

/// Read an `FSFileName` as a UTF-8 string.
fn file_name_string(name: &FSFileName) -> Option<String> {
    name.string().map(|s| s.to_string())
}

/// Join a directory path and a child name into a normalized absolute path.
fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Map a backend error to a POSIX errno.
fn errno(e: &StorageError) -> i32 {
    match e {
        StorageError::NotFound => libc::ENOENT,
        StorageError::NotADirectory => libc::ENOTDIR,
        StorageError::NotAFile => libc::EISDIR,
        StorageError::InvalidPath(_) => libc::EINVAL,
        StorageError::Backend(_) => libc::EIO,
    }
}

/// A POSIX-domain `NSError` for the given errno.
///
/// Returned by value so it outlives the reply-block call: pass
/// `Retained::as_ptr(&err(code)) as *mut NSError`, where the `err(code)`
/// temporary lives to the end of the statement — i.e. across `reply.call`.
fn err(code: i32) -> Retained<NSError> {
    // SAFETY: standard NSError factory; domain string and nil userInfo are valid.
    unsafe {
        NSError::errorWithDomain_code_userInfo(
            &NSString::from_str("NSPOSIXErrorDomain"),
            code as isize,
            None,
        )
    }
}
