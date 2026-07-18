//! Hand-written `objc2` bindings for the FSKit classes and protocols we use.
//!
//! FSKit ships no Rust bindings, so these mirror the ObjC headers directly. Only
//! the surface the extension touches is bound; expand as more operations are
//! implemented. Types are declared with `extern_class!`; the methods we call
//! with `extern_methods!`; the protocols we conform to with `extern_protocol!`.

#![allow(non_snake_case)]
// extern_protocol! generates `pub unsafe trait`s; clippy's missing_safety_doc
// fires on the macro expansion regardless of our own docs, so allow it here.
#![allow(clippy::missing_safety_doc)]

use block2::DynBlock;
use objc2::runtime::NSObject;
use objc2::runtime::NSObjectProtocol;
use objc2::{extern_class, extern_methods, extern_protocol, rc::Allocated, rc::Retained};
use objc2_foundation::{NSError, NSString, NSUUID};

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

impl FSEntityIdentifier {
    extern_methods!(
        #[unsafe(method(initWithUUID:))]
        pub fn initWithUUID(this: Allocated<Self>, uuid: &NSUUID) -> Retained<Self>;
    );
}

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

        #[unsafe(method(notRecognizedProbeResult))]
        pub fn notRecognized() -> Retained<FSProbeResult>;
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

// ---- protocols ---------------------------------------------------------------

extern_protocol!(
    /// `FSUnaryFileSystemOperations` — the delegate protocol our
    /// `FSUnaryFileSystem` subclass conforms to.
    ///
    /// # Safety
    ///
    /// An FSKit protocol binding: implementors must honor FSKit's calling
    /// contract — invoke each reply handler exactly once with either a valid
    /// object or an `NSError`, never both `nil`.
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
