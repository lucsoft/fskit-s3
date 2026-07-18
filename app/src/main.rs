//! `fskit-s3-app` — the macOS app for managing fskit-s3 connections and mounts.
//!
//! A status-bar app whose dropdown lists the configured **connections** (each
//! with a *Mount* action) and the currently mounted **volumes** (each with an
//! *Unmount* action), plus *Quit*. The menu rebuilds itself every time it opens
//! (via the `NSMenuDelegate` hook), so the mount list is always current — no
//! manual refresh. It owns the whole stack:
//!
//! - [`connection`] — the `Connection`/`ConnectionKind`/`Registry` model (a
//!   connection is a mountable endpoint; today only the in-memory demo, held in
//!   an in-memory registry).
//! - [`mounts`] — the mount table + `mount`/`unmount` actions. There is no bespoke
//!   CLI: mounting is the system `mount` tool with the connection's `-o` options,
//!   so the app (and a human at a terminal) drive the same command.
//! - [`appkit`] — checked wrappers over the AppKit calls, where the FFI lives.
//!
//! The `connection`/`mounts` modules are pure Rust and unit-tested; the app just
//! adds the UI. Runs as an `Accessory` app (no Dock tile). A connection-config UI
//! (add/edit S3 endpoints) arrives with real connections — there's nothing to
//! configure while the only connection is the built-in demo.

// The app must not panic in normal operation: no unwrap/expect/panic/indexing
// outside tests. Enforced by clippy in CI.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::unreachable
    )
)]

mod appkit;
mod connection;
mod mounts;

use connection::Registry;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};

/// Rust state carried on the ObjC controller instance.
struct Ivars {
    status_item: Retained<NSStatusItem>,
    /// The configured connections (in-memory for now; see [`connection`]).
    registry: Registry,
}

define_class!(
    // A plain NSObject subclass that serves as the menu's delegate + action handler.
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
            if let Err(e) = mounts::mount(conn, &conn.default_mount_point()) {
                eprintln!("[app] mount {name} failed: {e}");
            }
        }

        #[unsafe(method(unmount:))]
        fn unmount_action(&self, sender: Option<&NSMenuItem>) {
            let Some(item) = sender else { return };
            let Some(path) = appkit::represented_string(item) else { return };
            if let Err(e) = mounts::unmount(&path) {
                eprintln!("[app] unmount {path} failed: {e}");
            }
        }

        #[unsafe(method(quit:))]
        fn quit_action(&self, _sender: Option<&AnyObject>) {
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).terminate(None);
            }
        }
    }

    unsafe impl NSObjectProtocol for Controller {}

    // The menu asks its delegate to rebuild it right before each display, so the
    // contents are always current without a manual refresh action.
    unsafe impl NSMenuDelegate for Controller {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.populate(menu);
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker, status_item: Retained<NSStatusItem>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            status_item,
            registry: Registry::with_defaults(),
        });
        // SAFETY: `this` is a fresh alloc()+set_ivars, not yet initialized — the precondition of NSObject's designated `-init`.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        // Attach an (empty) menu whose delegate is this controller; it fills in
        // `menuNeedsUpdate:` on every open, so the list is always fresh.
        let menu = appkit::menu(mtm);
        appkit::set_menu_delegate(&menu, ProtocolObject::from_ref(&*this));
        appkit::set_menu(&this.ivars().status_item, &menu);
        this
    }

    /// Fill `menu` with the current connections + mounts. Called by the delegate
    /// before every display.
    fn populate(&self, menu: &NSMenu) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        appkit::clear_menu(menu);
        let target: &AnyObject = self;

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
        let mounted = mounts::list_fskit();
        if mounted.is_empty() {
            menu.addItem(&appkit::menu_item(mtm, "None", None, None, None, false));
        } else {
            for m in &mounted {
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
            "Quit",
            Some(sel!(quit:)),
            Some(target),
            None,
            true,
        ));
    }
}

fn main() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("[app] must run on the main thread");
        return;
    };

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    appkit::set_status_title(&status_item, "☁", mtm);

    // The controller owns the status item + menu; keep it alive for the whole run.
    let controller = Controller::new(mtm, status_item);

    app.run();
    drop(controller);
}
