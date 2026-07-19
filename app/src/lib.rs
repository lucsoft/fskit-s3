//! `fskit-s3-app` — the macOS app for managing fskit-s3 connections and mounts.
//!
//! A status-bar app whose dropdown offers *New Connection…*, lists the configured
//! **connections** — each a submenu carrying a *Mount*/*Unmount* toggle and an
//! *Update…* action, with a status dot (green when mounted, grey when not) — plus
//! *Quit*. The menu rebuilds itself on every open (via the `NSMenuDelegate` hook)
//! and reloads the registry from disk, so it's always current. It owns the whole
//! stack:
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
mod autostart;
mod connection;
mod devlog;
mod health;
mod healthwindow;
mod keychain;
mod mounts;
mod s3check;

use connection::Registry;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu, NSMenuDelegate,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

/// Rust state carried on the ObjC controller instance. The connection registry is
/// *not* held here — it's reloaded from disk on each menu open + mount action, so
/// changes made in the Add-mount window are always reflected.
struct Ivars {
    status_item: Retained<NSStatusItem>,
    /// The last extension-health check, shown on the menu's health row and reflected
    /// in the menu-bar glyph. Refreshed on launch and on every menu display.
    /// `RefCell`/`Cell` are safe here — the controller is `MainThreadOnly`, so this
    /// is only ever touched on the main thread.
    health: RefCell<health::Report>,
    /// Whether the health window has already been auto-raised once (so a not-ready
    /// extension prompts the user at launch, but not on every later refresh).
    raised: Cell<bool>,
    /// Hand-off slot for an async health check: the background thread stores its
    /// `(report, auto_raise)` here, then wakes the main thread (`applyPending:`),
    /// which drains it and applies the result. `Arc<Mutex>` because it crosses the
    /// thread boundary (the health check must not block the UI).
    pending: Arc<Mutex<Option<(health::Report, bool)>>>,
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
                if let Some(mtm) = MainThreadMarker::new() {
                    let title = format!("Couldn't mount “{name}”");
                    match classify_mount_error(&e) {
                        // Missing/unreadable secret on an S3 mount: offer the prompt
                        // (which mounts with `-o secret`) — the usual dev-build fix.
                        MountFailure::Secret if conn.is_s3() => {
                            if appkit::confirm(mtm, &title, &secret_hint(&e), "Enter Secret…") {
                                addwindow::open_password(mtm, conn.clone());
                            }
                        }
                        other => appkit::show_error(mtm, &title, &other.describe(&e)),
                    }
                }
            }
        }

        #[unsafe(method(addMount:))]
        fn add_mount_action(&self, _sender: Option<&AnyObject>) {
            if let Some(mtm) = MainThreadMarker::new() {
                addwindow::open(mtm);
            }
        }

        #[unsafe(method(health:))]
        fn health_action(&self, _sender: Option<&AnyObject>) {
            if let Some(mtm) = MainThreadMarker::new() {
                healthwindow::open(mtm);
            }
        }

        // Main-thread continuation of `refresh_health`: drain the slot the background
        // health check filled and apply it. Invoked via `performSelectorOnMainThread:`.
        #[unsafe(method(applyPending:))]
        fn apply_pending(&self, _sender: Option<&AnyObject>) {
            let taken = self
                .ivars()
                .pending
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if let Some((report, auto_raise)) = taken {
                self.apply_health(report, auto_raise);
            }
        }

        #[unsafe(method(update:))]
        fn update_action(&self, sender: Option<&NSMenuItem>) {
            let Some(item) = sender else { return };
            let Some(name) = appkit::represented_string(item) else {
                return;
            };
            let registry = Registry::load();
            let Some(conn) = registry.get(&name) else { return };
            if let Some(mtm) = MainThreadMarker::new() {
                addwindow::open_edit(mtm, conn.clone());
            }
        }

        #[unsafe(method(unmount:))]
        fn unmount_action(&self, sender: Option<&NSMenuItem>) {
            let Some(item) = sender else { return };
            let Some(path) = appkit::represented_string(item) else { return };
            if let Err(e) = mounts::unmount(&path) {
                eprintln!("[app] unmount {path} failed: {e}");
                if let Some(mtm) = MainThreadMarker::new() {
                    appkit::show_error(mtm, "Couldn’t unmount", &e);
                }
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

    // Clean up on quit (menu Quit, ⌘Q, or logout all route through here): unmount
    // our volumes so the extension isn't left serving a mount that later orphans an
    // fskitd record. The arg is an NSNotification we don't use, typed as AnyObject
    // to avoid the extra objc2-foundation feature.
    unsafe impl NSApplicationDelegate for Controller {
        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _notification: &AnyObject) {
            unmount_all_on_quit();
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker, status_item: Retained<NSStatusItem>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            status_item,
            health: RefCell::new(health::Report::initial()),
            raised: Cell::new(false),
            pending: Arc::new(Mutex::new(None)),
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

    /// Kick an extension-health check **without blocking the UI**: the (latency-bound)
    /// FSKit query runs on a background thread, and its result is applied on the main
    /// thread in `applyPending:`. Returns immediately, so the menu-bar glyph and the
    /// menu's health row update when the check completes rather than stalling on it.
    /// When `auto_raise` is set and the extension isn't ready, the health window is
    /// raised once — the launch-time nudge to the System Settings toggle (macOS won't
    /// let the app flip it).
    fn refresh_health(&self, auto_raise: bool) {
        let pending = self.ivars().pending.clone();
        let target: &AnyObject = self;
        appkit::run_off_main_then_notify(target, sel!(applyPending:), move || {
            let report = health::check();
            if let Ok(mut slot) = pending.lock() {
                *slot = Some((report, auto_raise));
            }
        });
    }

    /// Apply a completed health check on the main thread: update the menu-bar glyph,
    /// cache the report for the menu's health row, and raise the health window if the
    /// extension isn't ready and this is the launch-time check.
    fn apply_health(&self, report: health::Report, auto_raise: bool) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let glyphs = healthwindow::menu_glyphs(&report);
        appkit::set_status_symbol(&self.ivars().status_item, glyphs.bar_symbol, mtm);
        let ready = report.health.is_ready();
        *self.ivars().health.borrow_mut() = report;

        if auto_raise && !ready && !self.ivars().raised.get() {
            self.ivars().raised.set(true);
            healthwindow::open(mtm);
        }
    }

    /// Fill `menu` with the current connections + mounts. Called by the delegate
    /// before every display.
    fn populate(&self, menu: &NSMenu) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        appkit::clear_menu(menu);
        let target: &AnyObject = self;

        // Extension health — the top row (a dot + short status), reflecting the
        // FSKit extension's installed/enabled state (and any build mismatch). The row
        // shows the last-known state (from launch or the previous open) and kicks a
        // fresh async check whose result updates the glyph — and the row on the next
        // open — without blocking the menu. Clicking it opens the health window.
        self.refresh_health(false);
        let (row_symbol, row_tint, row_text) = {
            let report = self.ivars().health.borrow();
            let g = healthwindow::menu_glyphs(&report);
            (g.row_symbol, g.row_tint, g.row_text)
        };
        let health_item = appkit::menu_item(
            mtm,
            &row_text,
            Some(sel!(health:)),
            Some(target),
            None,
            true,
        );
        appkit::set_menu_item_symbol(&health_item, row_symbol, row_tint);
        menu.addItem(&health_item);
        menu.addItem(&appkit::separator(mtm));

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

        // Connections — each a dropdown with a Mount/Unmount toggle and an Update
        // action, prefixed with a status dot (green when mounted, grey when not).
        // Reloaded from disk so the Add/Edit window's changes show up immediately.
        menu.addItem(&appkit::menu_item(
            mtm,
            "Connections",
            None,
            None,
            None,
            false,
        ));
        let mounted = mounts::list_fskit();
        for c in Registry::load().list() {
            let mount_point = c.default_mount_point();
            let mount_point = mount_point.to_string_lossy();
            let is_mounted = mounted.iter().any(|m| m.mount_point == *mount_point);
            // A filled green circle when mounted, a hollow grey one when not.
            let (dot_symbol, dot_tint) = if is_mounted {
                ("circle.fill", appkit::Tint::Green)
            } else {
                ("circle", appkit::Tint::Secondary)
            };

            let submenu = appkit::menu(mtm);
            if is_mounted {
                submenu.addItem(&appkit::menu_item(
                    mtm,
                    "Unmount",
                    Some(sel!(unmount:)),
                    Some(target),
                    Some(&mount_point),
                    true,
                ));
            } else {
                submenu.addItem(&appkit::menu_item(
                    mtm,
                    "Mount",
                    Some(sel!(mount:)),
                    Some(target),
                    Some(&c.name),
                    true,
                ));
            }
            submenu.addItem(&appkit::menu_item(
                mtm,
                "Update…",
                Some(sel!(update:)),
                Some(target),
                Some(&c.name),
                true,
            ));

            let row = appkit::menu_item(
                mtm,
                &format!("{}  ({})", c.name, c.kind.label()),
                None,
                None,
                None,
                true,
            );
            appkit::set_menu_item_symbol(&row, dot_symbol, dot_tint);
            appkit::set_submenu(&row, &submenu);
            menu.addItem(&row);
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

/// A classified mount failure, so the error dialog can give a specific fix rather
/// than echo `mount`'s terse text.
enum MountFailure {
    /// fskitd still holds a mount-point record from a prior mount that didn't
    /// unmount cleanly (`Failed to store the mount point … Code=516`). Only a
    /// daemon restart clears it, and fskitd is root-owned — so admin is required.
    StaleRecord,
    /// The mount point is already in use (`Resource busy`).
    Busy,
    /// The extension rejected the mount (EINVAL) — most often a missing/unreadable
    /// S3 secret.
    Secret,
    /// Anything else — show the raw error.
    Other,
}

/// Classify a `mount` failure from its stderr text.
fn classify_mount_error(err: &str) -> MountFailure {
    if err.contains("already exists") || err.contains("Code=516") {
        MountFailure::StaleRecord
    } else if err.contains("Resource busy") {
        MountFailure::Busy
    } else if err.contains("Invalid argument") || err.contains("Code=22") {
        MountFailure::Secret
    } else {
        MountFailure::Other
    }
}

impl MountFailure {
    /// The informative text for the error dialog.
    fn describe(&self, err: &str) -> String {
        match self {
            MountFailure::StaleRecord => format!(
                "A leftover FSKit mount record is blocking this mount point — a \
                 previous mount didn't unmount cleanly. Clearing it needs a daemon \
                 restart (fskitd runs as root). In Terminal, run:\n\n    \
                 sudo killall fskitd\n\nthen try mounting again.\n\nDetails: {err}"
            ),
            MountFailure::Busy => format!(
                "Something is already mounted at this location. Unmount it first, \
                 then try again.\n\nDetails: {err}"
            ),
            MountFailure::Secret => secret_hint(err),
            MountFailure::Other => format!("Details: {err}"),
        }
    }
}

/// The dialog text for a likely secret/config rejection (EINVAL).
fn secret_hint(err: &str) -> String {
    format!(
        "The extension rejected the mount. If the secret is saved to the Keychain, \
         note that an unsigned/development build can't read the shared Keychain from \
         the extension — choose “Enter Secret…” to provide it for this mount. \
         Otherwise, check the connection's config.\n\nDetails: {err}"
    )
}

/// Cleanly unmount every volume this app serves, on quit. A clean (non-force)
/// unmount removes fskitd's mount-point record, which prevents the `Code=516`
/// "already exists" orphan a later crash/kill would otherwise leave behind. Uses
/// non-force so a busy volume (open files) stays mounted rather than being yanked.
/// Best-effort: failures are logged, not fatal.
fn unmount_all_on_quit() {
    for m in mounts::list_fskit() {
        // Only our own filesystem type — never touch another fskit module's mounts.
        if m.fs_type != mounts::FS_TYPE {
            continue;
        }
        if let Err(e) = mounts::unmount(&m.mount_point) {
            eprintln!("[app] unmount-on-quit {} failed: {e}", m.mount_point);
        }
    }
}

/// The extension-host entry point, called by the Xcode `fskit-s3-host` target's
/// Swift `@main` (`fskit_s3_app_run`). The host app is a thin shell whose only
/// job is to carry the embedded FSKit `.appex` and hand control to this Rust app;
/// all UI + logic lives here. Kept `extern "C"` + `#[no_mangle]` so the Swift
/// bootstrap can call it directly, mirroring how `ext` exports
/// `fskit_s3_make_filesystem`.
///
/// # Safety
/// Must be called exactly once, on the process's main thread (AppKit's
/// requirement); [`run`] enforces the main-thread check internally.
#[no_mangle]
pub extern "C" fn fskit_s3_app_run() {
    run();
}

/// Start the status-bar app: install the menu, register for login-at-launch,
/// raise the health window if the extension isn't ready, auto-mount flagged
/// connections, and run the AppKit event loop. Returns when the app terminates.
pub fn run() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("[app] must run on the main thread");
        return;
    };

    // Debug builds tail the extension's unified log to this terminal.
    devlog::start();

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // Give text fields the standard ⌘X/C/V/A (needs a main menu, even hidden).
    appkit::install_edit_menu(mtm);

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    appkit::set_status_symbol(&status_item, "cloud", mtm);

    // The controller owns the status item + menu; keep it alive for the whole run.
    let controller = Controller::new(mtm, status_item);
    // Also the app delegate, so `applicationWillTerminate:` can unmount on quit.
    app.setDelegate(Some(ProtocolObject::from_ref(&*controller)));

    // Register as a login item so the app relaunches at login (best-effort — a
    // failure here just means no auto-start, not a broken app).
    autostart::enable();

    // Kick an initial health check; when it resolves, the controller updates the
    // menu-bar glyph and, if the extension isn't enabled yet, raises the health
    // window to walk the user to the System Settings toggle (the one step macOS
    // won't let the app do itself).
    controller.refresh_health(true);

    mount_on_launch();

    app.run();
    drop(controller);
}
