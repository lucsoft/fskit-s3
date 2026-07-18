//! `fskit-s3-menubar` — a macOS status-bar app to manage fskit-s3 mounts.
//!
//! Shows a menu-bar item whose dropdown lists the configured connections (each
//! with a *Mount* action) and the currently mounted fskit-s3 volumes (each with
//! an *Unmount* action), plus *Refresh* and *Quit*. All connection/mount logic is
//! the shared [`fskit_s3_manage`] crate — the same one the `fskit-s3` CLI uses —
//! so this file is purely the AppKit UI, driven via `objc2` with all FFI confined
//! to [`appkit`].
//!
//! Runs as an `Accessory` app (no Dock tile).

// The app must not panic in normal operation.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

mod appkit;

use fskit_s3_manage::{self as manage, Registry};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};

/// Rust state carried on the ObjC controller instance.
struct Ivars {
    status_item: Retained<NSStatusItem>,
    /// The configured connections (in-memory for now; see `fskit_s3_manage`).
    registry: Registry,
}

define_class!(
    // A plain NSObject subclass that serves as the menu's target/action handler.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FskitS3MenuController"]
    #[ivars = Ivars]
    struct Controller;

    impl Controller {
        #[unsafe(method(mount:))]
        fn mount_action(&self, sender: Option<&NSMenuItem>) {
            let Some(item) = sender else { return };
            let Some(name) = appkit::represented_string(item) else {
                return;
            };
            let Some(conn) = self.ivars().registry.get(&name) else {
                return;
            };
            if let Err(e) = manage::mount(conn, &conn.default_mount_point()) {
                eprintln!("[menubar] mount {name} failed: {e}");
            }
            self.rebuild();
        }

        #[unsafe(method(unmount:))]
        fn unmount_action(&self, sender: Option<&NSMenuItem>) {
            let Some(item) = sender else { return };
            let Some(path) = appkit::represented_string(item) else { return };
            if let Err(e) = manage::unmount(&path) {
                eprintln!("[menubar] unmount {path} failed: {e}");
            }
            self.rebuild();
        }

        #[unsafe(method(refresh:))]
        fn refresh_action(&self, _sender: Option<&AnyObject>) {
            self.rebuild();
        }

        #[unsafe(method(quit:))]
        fn quit_action(&self, _sender: Option<&AnyObject>) {
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).terminate(None);
            }
        }
    }

    unsafe impl NSObjectProtocol for Controller {}
);

impl Controller {
    fn new(mtm: MainThreadMarker, status_item: Retained<NSStatusItem>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            status_item,
            registry: Registry::with_defaults(),
        });
        // SAFETY: standard NSObject designated initializer on a fresh allocation.
        unsafe { msg_send![super(this), init] }
    }

    /// Rebuild the dropdown from the current connections + mount list.
    fn rebuild(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let target: &AnyObject = self;
        let menu = appkit::menu(mtm);

        // Connections — each mountable.
        menu.addItem(&appkit::menu_item(
            mtm,
            "Connections",
            None,
            None,
            None,
            false,
        ));
        for c in self.ivars().registry.list() {
            menu.addItem(&appkit::menu_item(
                mtm,
                &format!("Mount  {}  ({})", c.name, c.kind.label()),
                Some(sel!(mount:)),
                Some(target),
                Some(&c.name),
                true,
            ));
        }

        menu.addItem(&appkit::separator(mtm));

        // Active mounts — each unmountable.
        menu.addItem(&appkit::menu_item(mtm, "Mounted", None, None, None, false));
        let mounts = manage::list_fskit();
        if mounts.is_empty() {
            menu.addItem(&appkit::menu_item(mtm, "None", None, None, None, false));
        } else {
            for m in &mounts {
                menu.addItem(&appkit::menu_item(
                    mtm,
                    &format!("Unmount  {}", m.mount_point),
                    Some(sel!(unmount:)),
                    Some(target),
                    Some(&m.mount_point),
                    true,
                ));
            }
        }

        menu.addItem(&appkit::separator(mtm));
        menu.addItem(&appkit::menu_item(
            mtm,
            "Refresh",
            Some(sel!(refresh:)),
            Some(target),
            None,
            true,
        ));
        menu.addItem(&appkit::menu_item(
            mtm,
            "Quit",
            Some(sel!(quit:)),
            Some(target),
            None,
            true,
        ));

        appkit::set_menu(&self.ivars().status_item, &menu);
    }
}

fn main() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("[menubar] must run on the main thread");
        return;
    };

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    appkit::set_status_title(&status_item, "☁", mtm);

    let controller = Controller::new(mtm, status_item);
    controller.rebuild();

    // The controller owns the status item; keep it alive for the whole run.
    app.run();
    drop(controller);
}
