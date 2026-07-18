//! Hand-written `objc2` bindings for the FSKit classes and protocols we use.
//!
//! FSKit ships no Rust bindings, so these mirror the ObjC headers directly. Types
//! are declared with `extern_class!`, the methods we call with `extern_methods!`,
//! and the protocols we conform to with `extern_protocol!`.

#![allow(non_snake_case)]
// extern_protocol! generates `pub unsafe trait`s; clippy's missing_safety_doc
// fires on the macro expansion regardless of our own docs, so allow it here.
#![allow(clippy::missing_safety_doc)]

use core::ffi::c_void;

use block2::DynBlock;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{extern_class, extern_methods, extern_protocol, rc::Allocated, rc::Retained};
use objc2_foundation::{NSData, NSError, NSString, NSUUID};

// ---- scalar typedefs (mirror FSKit's NS_ENUM/NS_OPTIONS) ---------------------

/// `FSItemType` (NS_ENUM(NSInteger)).
pub type FSItemType = isize;
pub const FS_ITEM_TYPE_FILE: FSItemType = 1;
pub const FS_ITEM_TYPE_DIRECTORY: FSItemType = 2;

/// `FSItemID` (NS_ENUM(UInt64)).
pub type FSItemID = u64;
pub const FS_ITEM_ID_ROOT_DIRECTORY: FSItemID = 2;

pub type FSDirectoryCookie = u64;
pub type FSDirectoryVerifier = u64;
/// `FSSyncFlags` / `FSDeactivateOptions` (NS_OPTIONS(NSUInteger)).
pub type FSSyncFlags = usize;
pub type FSDeactivateOptions = usize;

// ---- opaque passthrough types ------------------------------------------------

extern_class!(
    /// A resource FSKit asks us to probe/load (for us, effectively a handle).
    #[unsafe(super(NSObject))]
    #[name = "FSResource"]
    pub struct FSResource;
);

extern_class!(
    /// Mount/load options (`-f`, `--rdonly`, …). Opaque to us for now.
    #[unsafe(super(NSObject))]
    #[name = "FSTaskOptions"]
    pub struct FSTaskOptions;
);

// ---- identifiers -------------------------------------------------------------

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSEntityIdentifier"]
    pub struct FSEntityIdentifier;
);

extern_class!(
    #[unsafe(super(FSEntityIdentifier))]
    #[name = "FSContainerIdentifier"]
    pub struct FSContainerIdentifier;
);

impl FSContainerIdentifier {
    extern_methods!(
        #[unsafe(method(initWithUUID:))]
        pub fn initWithUUID(this: Allocated<Self>, uuid: &NSUUID) -> Retained<Self>;
    );
}

extern_class!(
    #[unsafe(super(FSEntityIdentifier))]
    #[name = "FSVolumeIdentifier"]
    pub struct FSVolumeIdentifier;
);

impl FSVolumeIdentifier {
    extern_methods!(
        #[unsafe(method(initWithUUID:))]
        pub fn initWithUUID(this: Allocated<Self>, uuid: &NSUUID) -> Retained<Self>;
    );
}

// ---- probe result ------------------------------------------------------------

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSProbeResult"]
    pub struct FSProbeResult;
);

impl FSProbeResult {
    extern_methods!(
        #[unsafe(method(usableProbeResultWithName:containerID:))]
        pub fn usable(
            name: &NSString,
            container_id: &FSContainerIdentifier,
        ) -> Retained<FSProbeResult>;
    );
}

// ---- file name ---------------------------------------------------------------

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSFileName"]
    pub struct FSFileName;
);

impl FSFileName {
    extern_methods!(
        #[unsafe(method(nameWithString:))]
        pub fn nameWithString(name: &NSString) -> Retained<FSFileName>;

        #[unsafe(method(string))]
        pub fn string(&self) -> Option<Retained<NSString>>;
    );
}

// ---- items + attributes ------------------------------------------------------

extern_class!(
    /// Base item class; we subclass it (see `item.rs`) to carry a path.
    #[unsafe(super(NSObject))]
    #[name = "FSItem"]
    pub struct FSItem;
);

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSItemAttributes"]
    pub struct FSItemAttributes;
);

impl FSItemAttributes {
    extern_methods!(
        #[unsafe(method(new))]
        pub fn new() -> Retained<FSItemAttributes>;

        #[unsafe(method(setType:))]
        pub fn setType(&self, value: FSItemType);
        #[unsafe(method(setMode:))]
        pub fn setMode(&self, value: u32);
        #[unsafe(method(setLinkCount:))]
        pub fn setLinkCount(&self, value: u32);
        #[unsafe(method(setUid:))]
        pub fn setUid(&self, value: u32);
        #[unsafe(method(setGid:))]
        pub fn setGid(&self, value: u32);
        #[unsafe(method(setSize:))]
        pub fn setSize(&self, value: u64);
        #[unsafe(method(setAllocSize:))]
        pub fn setAllocSize(&self, value: u64);
        #[unsafe(method(setFileID:))]
        pub fn setFileID(&self, value: FSItemID);
        #[unsafe(method(setParentID:))]
        pub fn setParentID(&self, value: FSItemID);
    );
}

extern_class!(
    /// `FSItemGetAttributesRequest` — we populate all attributes regardless, so
    /// we never inspect `wantedAttributes`.
    #[unsafe(super(NSObject))]
    #[name = "FSItemGetAttributesRequest"]
    pub struct FSItemGetAttributesRequest;
);

extern_class!(
    #[unsafe(super(FSItemAttributes))]
    #[name = "FSItemSetAttributesRequest"]
    pub struct FSItemSetAttributesRequest;
);

// ---- directory enumeration + read buffer -------------------------------------

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSDirectoryEntryPacker"]
    pub struct FSDirectoryEntryPacker;
);

impl FSDirectoryEntryPacker {
    extern_methods!(
        #[unsafe(method(packEntryWithName:itemType:itemID:nextCookie:attributes:))]
        pub fn packEntry(
            &self,
            name: &FSFileName,
            item_type: FSItemType,
            item_id: FSItemID,
            next_cookie: FSDirectoryCookie,
            attributes: Option<&FSItemAttributes>,
        ) -> bool;
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSMutableFileDataBuffer"]
    pub struct FSMutableFileDataBuffer;
);

impl FSMutableFileDataBuffer {
    extern_methods!(
        #[unsafe(method(mutableBytes))]
        pub fn mutableBytes(&self) -> *mut c_void;
        #[unsafe(method(length))]
        pub fn length(&self) -> usize;
    );
}

// ---- volume capabilities + statfs -------------------------------------------

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSVolumeSupportedCapabilities"]
    pub struct FSVolumeSupportedCapabilities;
);

impl FSVolumeSupportedCapabilities {
    extern_methods!(
        #[unsafe(method(new))]
        pub fn new() -> Retained<FSVolumeSupportedCapabilities>;
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSStatFSResult"]
    pub struct FSStatFSResult;
);

impl FSStatFSResult {
    extern_methods!(
        #[unsafe(method(initWithFileSystemTypeName:))]
        pub fn initWithFileSystemTypeName(this: Allocated<Self>, name: &NSString)
            -> Retained<Self>;

        #[unsafe(method(setBlockSize:))]
        pub fn setBlockSize(&self, value: isize);
        #[unsafe(method(setTotalBlocks:))]
        pub fn setTotalBlocks(&self, value: u64);
        #[unsafe(method(setAvailableBlocks:))]
        pub fn setAvailableBlocks(&self, value: u64);
        #[unsafe(method(setFreeBlocks:))]
        pub fn setFreeBlocks(&self, value: u64);
        #[unsafe(method(setUsedBlocks:))]
        pub fn setUsedBlocks(&self, value: u64);
    );
}

// ---- volume + unary file system ---------------------------------------------

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSVolume"]
    pub struct FSVolume;
);

impl FSVolume {
    extern_methods!(
        #[unsafe(method(initWithVolumeID:volumeName:))]
        pub fn initWithVolumeID_volumeName(
            this: Allocated<Self>,
            volume_id: &FSVolumeIdentifier,
            volume_name: &FSFileName,
        ) -> Retained<Self>;
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSUnaryFileSystem"]
    pub struct FSUnaryFileSystem;
);

impl FSUnaryFileSystem {
    extern_methods!(
        // `containerStatus` (on FSFileSystemBase): loadResource must move the
        // container out of `notReady` or FSKit rejects it ("unexpected container
        // state").
        #[unsafe(method(setContainerStatus:))]
        pub fn setContainerStatus(&self, status: &FSContainerStatus);
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    #[name = "FSContainerStatus"]
    pub struct FSContainerStatus;
);

impl FSContainerStatus {
    extern_methods!(
        /// The `+ready` class property: state `ready`, nil status.
        #[unsafe(method(ready))]
        pub fn ready() -> Retained<FSContainerStatus>;

        /// The `+active` class property: state `active`, nil status.
        #[unsafe(method(active))]
        pub fn active() -> Retained<FSContainerStatus>;

        /// `+notReadyWithStatus:` — pass `None` for the nil-status not-ready state.
        #[unsafe(method(notReadyWithStatus:))]
        pub fn notReadyWithStatus(status: Option<&NSError>) -> Retained<FSContainerStatus>;
    );
}

// ---- protocols ---------------------------------------------------------------

extern_protocol!(
    /// `FSUnaryFileSystemOperations`.
    pub unsafe trait FSUnaryFileSystemOperations: NSObjectProtocol {
        #[unsafe(method(probeResource:replyHandler:))]
        unsafe fn probeResource_replyHandler(
            &self,
            resource: &FSResource,
            reply: &DynBlock<dyn Fn(*mut FSProbeResult, *mut NSError)>,
        );

        #[unsafe(method(loadResource:options:replyHandler:))]
        unsafe fn loadResource_options_replyHandler(
            &self,
            resource: &FSResource,
            options: &FSTaskOptions,
            reply: &DynBlock<dyn Fn(*mut FSVolume, *mut NSError)>,
        );

        #[unsafe(method(unloadResource:options:replyHandler:))]
        unsafe fn unloadResource_options_replyHandler(
            &self,
            resource: &FSResource,
            options: &FSTaskOptions,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        );
    }
);

extern_protocol!(
    /// `FSVolumePathConfOperations` — reported as pathconf limits.
    pub unsafe trait FSVolumePathConfOperations: NSObjectProtocol {
        #[unsafe(method(maximumLinkCount))]
        unsafe fn maximumLinkCount(&self) -> isize;
        #[unsafe(method(maximumNameLength))]
        unsafe fn maximumNameLength(&self) -> isize;
        #[unsafe(method(restrictsOwnershipChanges))]
        unsafe fn restrictsOwnershipChanges(&self) -> bool;
        #[unsafe(method(truncatesLongNames))]
        unsafe fn truncatesLongNames(&self) -> bool;
    }
);

extern_protocol!(
    /// `FSVolumeOperations` — the core volume delegate protocol.
    pub unsafe trait FSVolumeOperations: FSVolumePathConfOperations {
        #[unsafe(method(supportedVolumeCapabilities))]
        unsafe fn supportedVolumeCapabilities(&self) -> *mut FSVolumeSupportedCapabilities;
        #[unsafe(method(volumeStatistics))]
        unsafe fn volumeStatistics(&self) -> *mut FSStatFSResult;

        #[unsafe(method(mountWithOptions:replyHandler:))]
        unsafe fn mountWithOptions_replyHandler(
            &self,
            options: &FSTaskOptions,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        );
        #[unsafe(method(unmountWithReplyHandler:))]
        unsafe fn unmountWithReplyHandler(&self, reply: &DynBlock<dyn Fn()>);
        #[unsafe(method(synchronizeWithFlags:replyHandler:))]
        unsafe fn synchronizeWithFlags_replyHandler(
            &self,
            flags: FSSyncFlags,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        );
        #[unsafe(method(getAttributes:ofItem:replyHandler:))]
        unsafe fn getAttributes_ofItem_replyHandler(
            &self,
            desired: &FSItemGetAttributesRequest,
            item: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        );
        #[unsafe(method(setAttributes:onItem:replyHandler:))]
        unsafe fn setAttributes_onItem_replyHandler(
            &self,
            attrs: &FSItemSetAttributesRequest,
            item: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSItemAttributes, *mut NSError)>,
        );
        #[unsafe(method(lookupItemNamed:inDirectory:replyHandler:))]
        unsafe fn lookupItemNamed_inDirectory_replyHandler(
            &self,
            name: &FSFileName,
            directory: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        );
        #[unsafe(method(reclaimItem:replyHandler:))]
        unsafe fn reclaimItem_replyHandler(
            &self,
            item: &FSItem,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        );
        #[unsafe(method(readSymbolicLink:replyHandler:))]
        unsafe fn readSymbolicLink_replyHandler(
            &self,
            item: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        );
        #[unsafe(method(createItemNamed:type:inDirectory:attributes:replyHandler:))]
        unsafe fn createItemNamed_type_inDirectory_attributes_replyHandler(
            &self,
            name: &FSFileName,
            item_type: FSItemType,
            directory: &FSItem,
            attributes: &FSItemSetAttributesRequest,
            reply: &DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        );
        #[unsafe(method(createSymbolicLinkNamed:inDirectory:attributes:linkContents:replyHandler:))]
        unsafe fn createSymbolicLinkNamed_inDirectory_attributes_linkContents_replyHandler(
            &self,
            name: &FSFileName,
            directory: &FSItem,
            attributes: &FSItemSetAttributesRequest,
            contents: &FSFileName,
            reply: &DynBlock<dyn Fn(*mut FSItem, *mut FSFileName, *mut NSError)>,
        );
        #[unsafe(method(createLinkToItem:named:inDirectory:replyHandler:))]
        unsafe fn createLinkToItem_named_inDirectory_replyHandler(
            &self,
            item: &FSItem,
            name: &FSFileName,
            directory: &FSItem,
            reply: &DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        );
        #[unsafe(method(removeItem:named:fromDirectory:replyHandler:))]
        unsafe fn removeItem_named_fromDirectory_replyHandler(
            &self,
            item: &FSItem,
            name: &FSFileName,
            directory: &FSItem,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        );
        #[unsafe(method(renameItem:inDirectory:named:toNewName:inDirectory:overItem:replyHandler:))]
        #[allow(clippy::too_many_arguments)]
        unsafe fn renameItem_inDirectory_named_toNewName_inDirectory_overItem_replyHandler(
            &self,
            item: &FSItem,
            source_directory: &FSItem,
            source_name: &FSFileName,
            destination_name: &FSFileName,
            destination_directory: &FSItem,
            over_item: *mut FSItem,
            reply: &DynBlock<dyn Fn(*mut FSFileName, *mut NSError)>,
        );
        #[unsafe(method(enumerateDirectory:startingAtCookie:verifier:providingAttributes:usingPacker:replyHandler:))]
        unsafe fn enumerateDirectory_startingAtCookie_verifier_providingAttributes_usingPacker_replyHandler(
            &self,
            directory: &FSItem,
            cookie: FSDirectoryCookie,
            verifier: FSDirectoryVerifier,
            attributes: *mut FSItemGetAttributesRequest,
            packer: &FSDirectoryEntryPacker,
            reply: &DynBlock<dyn Fn(FSDirectoryVerifier, *mut NSError)>,
        );
        #[unsafe(method(activateWithOptions:replyHandler:))]
        unsafe fn activateWithOptions_replyHandler(
            &self,
            options: &FSTaskOptions,
            reply: &DynBlock<dyn Fn(*mut FSItem, *mut NSError)>,
        );
        #[unsafe(method(deactivateWithOptions:replyHandler:))]
        unsafe fn deactivateWithOptions_replyHandler(
            &self,
            options: FSDeactivateOptions,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        );
    }
);

extern_protocol!(
    /// `FSVolumeReadWriteOperations`.
    pub unsafe trait FSVolumeReadWriteOperations: NSObjectProtocol {
        #[unsafe(method(readFromFile:offset:length:intoBuffer:replyHandler:))]
        unsafe fn readFromFile_offset_length_intoBuffer_replyHandler(
            &self,
            item: &FSItem,
            offset: i64,
            length: usize,
            buffer: &FSMutableFileDataBuffer,
            reply: &DynBlock<dyn Fn(usize, *mut NSError)>,
        );
        #[unsafe(method(writeContents:toFile:atOffset:replyHandler:))]
        unsafe fn writeContents_toFile_atOffset_replyHandler(
            &self,
            contents: &NSData,
            item: &FSItem,
            offset: i64,
            reply: &DynBlock<dyn Fn(usize, *mut NSError)>,
        );
    }
);
