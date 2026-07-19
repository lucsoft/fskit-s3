//! Launch-at-login registration, via `SMAppService.mainApp` (ServiceManagement).
//!
//! Registering the main app as a login item makes macOS relaunch it after login,
//! so a mounted volume comes back on its own. This is the modern replacement for
//! the old `LSSharedFileList`/login-item plist dance: one call, and the user can
//! still revoke it in System Settings ▸ General ▸ Login Items.
//!
//! It only takes effect for a signed app installed in `/Applications` (the shipped
//! host); a standalone `cargo run` build fails to register, which is fine — this
//! is best-effort and never fatal.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

/// `SMAppServiceStatus` (`SMAppService.h`): the registration state of a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not registered with ServiceManagement (the initial state, or after unregister).
    NotRegistered,
    /// Registered and allowed to launch at login.
    Enabled,
    /// Registered but awaiting the user's approval in Login Items.
    RequiresApproval,
    /// The service (this bundle) couldn't be found — e.g. an unsigned dev build.
    NotFound,
    /// An unrecognized status value.
    Unknown,
}

impl Status {
    fn from_raw(raw: isize) -> Self {
        match raw {
            0 => Status::NotRegistered,
            1 => Status::Enabled,
            2 => Status::RequiresApproval,
            3 => Status::NotFound,
            _ => Status::Unknown,
        }
    }
}

/// The shared `SMAppService.mainApp` instance (the current app as a login item).
/// `None` if ServiceManagement is somehow unavailable.
fn main_app_service() -> Option<Retained<AnyObject>> {
    // SAFETY: `SMAppService` is a ServiceManagement class (linked via build.rs);
    // `mainAppService` is a readonly class property returning a `+0` autoreleased
    // instance, which `Retained::retain_autoreleased` takes ownership of.
    unsafe {
        let cls = class!(SMAppService);
        let svc: *mut AnyObject = msg_send![cls, mainAppService];
        Retained::retain(svc)
    }
}

/// Register the app to launch at login (best-effort). Logs the outcome; never
/// panics or propagates — a dev build that can't register just won't auto-start.
pub fn enable() {
    let Some(svc) = main_app_service() else {
        eprintln!("[app] autostart: ServiceManagement unavailable");
        return;
    };
    if status_of(&svc) == Status::Enabled {
        return; // already a login item; nothing to do
    }
    // SAFETY: `svc` is a live SMAppService; `registerAndReturnError:` takes a
    // nullable error out-param — we pass null and rely on the BOOL + status.
    let ok: bool =
        unsafe { msg_send![&svc, registerAndReturnError: core::ptr::null_mut::<*mut AnyObject>()] };
    if ok {
        eprintln!("[app] autostart: registered as a login item");
    } else {
        eprintln!(
            "[app] autostart: registration not applied (status: {:?}) — \
             expected for an unsigned/dev build",
            status_of(&svc)
        );
    }
}

/// The current login-item registration status.
pub fn current_status() -> Status {
    main_app_service()
        .map(|svc| status_of(&svc))
        .unwrap_or(Status::Unknown)
}

fn status_of(svc: &AnyObject) -> Status {
    // SAFETY: `status` is a readonly `SMAppServiceStatus` (NSInteger) property.
    let raw: isize = unsafe { msg_send![svc, status] };
    Status::from_raw(raw)
}
