//! `fskit-s3-app` — the macOS app for managing fskit-s3 connections and mounts.
//!
//! A status-bar app whose dropdown offers *Add mount…*, lists the configured
//! **connections** (each with a *Mount* action) and the currently mounted
//! **volumes** (each with an *Unmount* action), plus *Quit*. The menu rebuilds
//! itself on every open (via the `NSMenuDelegate` hook) and reloads the registry
//! from disk, so it's always current. It owns the whole stack:
//!
//! - [`connection`] — the `Connection`/`ConnectionKind` (`Memory` / `S3`) model +
//!   the persisted `Registry` (`connections.json`, never holding a secret).
//! - [`keychain`] — the S3 secret in the macOS Keychain (secure path).
//! - [`s3check`] — the "Test and Save" credential check (lists the bucket).
//! - [`mounts`] — the mount table + `mount`/`unmount`. No bespoke CLI: mounting is
//!   the system `mount` tool with the connection's `-o` options.
//! - [`addwindow`] — the Add-mount form + the password prompt (native `NSWindow`).
//! - [`appkit`] — checked wrappers over the AppKit calls, where the FFI lives.
//!
//! The `connection`/`keychain`/`s3check`/`mounts` modules are pure Rust; the app
//! adds the UI. Runs as an `Accessory` app (no Dock tile).

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

mod addwindow;
mod appkit;
mod connection;
mod keychain;
mod mounts;
mod s3check;

use connection::Registry;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};

/// Rust state carried on the ObjC controller instance. The connection registry is
/// *not* held here — it's reloaded from disk on each menu open + mount action, so
/// changes made in the Add-mount window are always reflected.
struct Ivars {
    status_item: Retained<NSStatusItem>,
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
            let registry = Registry::load();
            let Some(conn) = registry.get(&name) else { return };

            // S3 with no stored secret: prompt for it (the ext can't). Otherwise
            // mount with no `-o secret` — the ext reads the Keychain by name.
            if conn.is_s3() && keychain::read_secret(&name).is_none() {
                if let Some(mtm) = MainThreadMarker::new() {
                    addwindow::open_password(mtm, conn.clone());
                }
                return;
            }
            if let Err(e) = mounts::mount(conn, &conn.default_mount_point(), None) {
                eprintln!("[app] mount {name} failed: {e}");
            }
        }

        #[unsafe(method(addMount:))]
        fn add_mount_action(&self, _sender: Option<&AnyObject>) {
            if let Some(mtm) = MainThreadMarker::new() {
                addwindow::open(mtm);
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
        let this = Self::alloc(mtm).set_ivars(Ivars { status_item });
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

        // Add a new connection.
        menu.addItem(&appkit::menu_item(
            mtm,
            "New Connection…",
            Some(sel!(addMount:)),
            Some(target),
            None,
            true,
        ));
        menu.addItem(&appkit::separator(mtm));

        // Connections — each mountable. Reloaded from disk so the Add-mount window's
        // changes show up immediately.
        menu.addItem(&appkit::menu_item(
            mtm,
            "Connections",
            None,
            None,
            None,
            false,
        ));
        for c in Registry::load().list() {
            menu.addItem(&appkit::menu_item(
                mtm,
                &format!("Mount {}  ({})", c.name, c.kind.label()),
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
                    &format!("Unmount {}", m.mount_point),
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

/// Mount every connection flagged `mount_on_launch`. S3 connections whose secret
/// isn't in the Keychain are skipped (a prompt can't run unattended at launch).
fn mount_on_launch() {
    for conn in Registry::load().list() {
        if !conn.mount_on_launch {
            continue;
        }
        if conn.is_s3() && keychain::read_secret(&conn.name).is_none() {
            eprintln!("[app] skip auto-mount {}: no stored secret", conn.name);
            continue;
        }
        if let Err(e) = mounts::mount(conn, &conn.default_mount_point(), None) {
            eprintln!("[app] auto-mount {} failed: {e}", conn.name);
        }
    }
}

fn main() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("[app] must run on the main thread");
        return;
    };

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // Give text fields the standard ⌘X/C/V/A (needs a main menu, even hidden).
    appkit::install_edit_menu(mtm);

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    appkit::set_status_title(&status_item, "☁", mtm);

    // The controller owns the status item + menu; keep it alive for the whole run.
    let controller = Controller::new(mtm, status_item);

    mount_on_launch();

    app.run();
    drop(controller);
}
