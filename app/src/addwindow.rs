//! The "Add mount" window — create a connection (In-memory or S3).
//!
//! A native `NSWindow` form built via [`crate::appkit`] (all FFI stays there).
//! On **Test & Save** an S3 connection's credentials are validated by listing the
//! bucket ([`crate::s3check`]); on success the connection is persisted and, when
//! "Save secret to Keychain" is checked, the secret is stored via
//! [`crate::keychain`]. The menu picks up the new connection on its next open
//! (it reloads the registry from disk).

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSButton, NSPopUpButton, NSSecureTextField, NSTextField, NSView, NSWindow};

use crate::appkit;
use crate::connection::{Connection, ConnectionKind, Registry, S3Meta};

// Keeps the live window controller retained for as long as the window is open
// (main-thread only). Replaced — and the previous one dropped — on the next open.
thread_local! {
    static CURRENT: RefCell<Option<Retained<AddWindowController>>> = const { RefCell::new(None) };
}

/// Retained references to the window + the fields we read/toggle on save.
struct AddIvars {
    window: Retained<NSWindow>,
    type_popup: Retained<NSPopUpButton>,
    name: Retained<NSTextField>,
    s3_box: Retained<NSView>,
    endpoint: Retained<NSTextField>,
    bucket: Retained<NSTextField>,
    region: Retained<NSTextField>,
    akid: Retained<NSTextField>,
    secret: Retained<NSSecureTextField>,
    token: Retained<NSTextField>,
    save_keychain: Retained<NSButton>,
    mount_launch: Retained<NSButton>,
    status: Retained<NSTextField>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FskitS3AddWindowController"]
    #[ivars = AddIvars]
    struct AddWindowController;

    impl AddWindowController {
        #[unsafe(method(typeChanged:))]
        fn type_changed(&self, _sender: Option<&AnyObject>) {
            self.update_visibility();
        }

        #[unsafe(method(testAndSave:))]
        fn test_and_save(&self, _sender: Option<&AnyObject>) {
            self.on_save();
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            appkit::close_window(&self.ivars().window);
        }
    }

    unsafe impl NSObjectProtocol for AddWindowController {}
);

impl AddWindowController {
    fn new(mtm: MainThreadMarker, ivars: AddIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: `this` is a fresh alloc()+set_ivars, not yet initialized — the precondition of NSObject's designated `-init`.
        unsafe { msg_send![super(this), init] }
    }

    /// Show the S3 fields only for the S3 type, and shrink/grow the window height
    /// to fit (the top group stays put; the bottom group + buttons follow).
    fn update_visibility(&self) {
        let iv = self.ivars();
        let is_s3 = appkit::popup_index(&iv.type_popup) == 1;
        appkit::set_hidden(&iv.s3_box, !is_s3);
        let height = if is_s3 { WINDOW_H_S3 } else { WINDOW_H_MEMORY };
        appkit::set_window_content_size(&iv.window, WINDOW_W, height);
    }

    /// Validate (S3), persist the connection + secret, and close on success.
    fn on_save(&self) {
        let iv = self.ivars();
        let status = |msg: &str| appkit::set_string(&iv.status, msg);

        let name = appkit::field_string(&iv.name).trim().to_string();
        if name.is_empty() {
            status("Name is required.");
            return;
        }
        let mount_on_launch = appkit::checkbox_on(&iv.mount_launch);
        let is_s3 = appkit::popup_index(&iv.type_popup) == 1;

        let conn = if is_s3 {
            let token = appkit::field_string(&iv.token).trim().to_string();
            let meta = S3Meta {
                bucket: appkit::field_string(&iv.bucket).trim().to_string(),
                region: appkit::field_string(&iv.region).trim().to_string(),
                endpoint: appkit::field_string(&iv.endpoint).trim().to_string(),
                access_key_id: appkit::field_string(&iv.akid).trim().to_string(),
                session_token: (!token.is_empty()).then_some(token),
            };
            let secret = appkit::field_string(&iv.secret);
            if meta.bucket.is_empty() || meta.access_key_id.is_empty() || secret.is_empty() {
                status("Bucket, access key, and secret are required.");
                return;
            }
            // NOTE: runs the network check synchronously, so the UI briefly blocks
            // (fine for a local endpoint; a background thread is a later refinement).
            if let Err(e) = crate::s3check::test_s3(&meta, &secret) {
                status(&format!("Test failed: {e}"));
                return;
            }
            let save_keychain = appkit::checkbox_on(&iv.save_keychain);
            if save_keychain {
                if let Err(e) = crate::keychain::store_secret(&name, &secret) {
                    status(&format!("Keychain save failed: {e}"));
                    return;
                }
            }
            Connection {
                name: name.clone(),
                kind: ConnectionKind::S3(meta),
                save_secret_to_keychain: save_keychain,
                mount_on_launch,
            }
        } else {
            Connection {
                name: name.clone(),
                kind: ConnectionKind::Memory,
                save_secret_to_keychain: false,
                mount_on_launch,
            }
        };

        let mut registry = Registry::load();
        if registry.get(&name).is_some() {
            status(&format!("A connection named {name:?} already exists."));
            return;
        }
        if let Err(e) = registry.add(conn) {
            status(&e);
            return;
        }
        if let Err(e) = registry.save() {
            status(&format!("Save failed: {e}"));
            return;
        }
        appkit::close_window(&iv.window);
    }
}

/// Window content width, and the two heights the form toggles between (S3 fields
/// shown vs. hidden). The bottom group (checkbox/status/buttons) is pinned to the
/// bottom, so shrinking the window collapses the empty middle.
const WINDOW_W: f64 = 380.0;
const WINDOW_H_S3: f64 = 440.0;
const WINDOW_H_MEMORY: f64 = 240.0;

/// Open the Add-mount window (replacing any previous one). Controls are laid out
/// for the tall (S3) height; `update_visibility` shrinks to the memory height for
/// the default In-memory selection before the window is shown.
pub fn open(mtm: MainThreadMarker) {
    let window = appkit::make_window(mtm, WINDOW_W, WINDOW_H_S3, "Add mount");
    let Some(content) = appkit::content_view(&window) else {
        return;
    };
    // Top group tracks the top edge; bottom group tracks the bottom edge.
    let add_top = |v: &NSView| {
        appkit::pin_top(v);
        appkit::add_subview(&content, v);
    };
    let add_bottom = |v: &NSView| {
        appkit::pin_bottom(v);
        appkit::add_subview(&content, v);
    };

    // Name.
    add_top(&appkit::label(
        mtm,
        appkit::rect(20.0, 400.0, 120.0, 20.0),
        "Name",
    ));
    let name = appkit::text_field(mtm, appkit::rect(150.0, 398.0, 210.0, 22.0), "");
    add_top(&name);

    // Type selector.
    add_top(&appkit::label(
        mtm,
        appkit::rect(20.0, 368.0, 120.0, 20.0),
        "Type",
    ));
    let type_popup = appkit::popup(
        mtm,
        appkit::rect(150.0, 364.0, 210.0, 26.0),
        &["In-memory", "S3"],
    );
    add_top(&type_popup);

    // S3 fields, grouped in a box so the whole group toggles + moves at once.
    let s3_box = appkit::plain_view(mtm, appkit::rect(0.0, 150.0, 380.0, 205.0));
    let field_row = |label_text: &str, rel_y: f64| -> Retained<NSTextField> {
        appkit::add_subview(
            &s3_box,
            &appkit::label(
                mtm,
                appkit::rect(20.0, rel_y + 2.0, 120.0, 20.0),
                label_text,
            ),
        );
        let f = appkit::text_field(mtm, appkit::rect(150.0, rel_y, 210.0, 22.0), "");
        appkit::add_subview(&s3_box, &f);
        f
    };
    let endpoint = field_row("Endpoint", 178.0);
    let bucket = field_row("Bucket", 150.0);
    let region = field_row("Region", 122.0);
    let akid = field_row("Access Key ID", 94.0);
    appkit::add_subview(
        &s3_box,
        &appkit::label(mtm, appkit::rect(20.0, 68.0, 120.0, 20.0), "Secret"),
    );
    let secret = appkit::secure_field(mtm, appkit::rect(150.0, 66.0, 210.0, 22.0));
    appkit::add_subview(&s3_box, &secret);
    let token = field_row("Session token", 38.0);
    let save_keychain = appkit::checkbox(
        mtm,
        appkit::rect(150.0, 8.0, 210.0, 20.0),
        "Save secret to Keychain",
    );
    appkit::add_subview(&s3_box, &save_keychain);
    add_top(&s3_box);

    // Always-visible options + status + buttons (pinned to the bottom edge).
    let mount_launch = appkit::checkbox(
        mtm,
        appkit::rect(150.0, 118.0, 210.0, 20.0),
        "Mount when launching",
    );
    add_bottom(&mount_launch);
    let status = appkit::label(mtm, appkit::rect(20.0, 56.0, 340.0, 44.0), "");
    add_bottom(&status);
    let cancel = appkit::push_button(mtm, appkit::rect(150.0, 14.0, 100.0, 32.0), "Cancel");
    add_bottom(&cancel);
    let save = appkit::push_button(mtm, appkit::rect(256.0, 14.0, 104.0, 32.0), "Test & Save");
    add_bottom(&save);

    let controller = AddWindowController::new(
        mtm,
        AddIvars {
            window: window.clone(),
            type_popup: type_popup.clone(),
            name: name.clone(),
            s3_box: s3_box.clone(),
            endpoint,
            bucket,
            region,
            akid,
            secret: secret.clone(),
            token,
            save_keychain: save_keychain.clone(),
            mount_launch: mount_launch.clone(),
            status: status.clone(),
        },
    );

    // Wire actions now that the target exists (same underlying objects as ivars).
    let target: &AnyObject = &controller;
    appkit::set_target_action(&type_popup, sel!(typeChanged:), target);
    appkit::set_target_action(&save, sel!(testAndSave:), target);
    appkit::set_target_action(&cancel, sel!(cancel:), target);

    controller.update_visibility();

    CURRENT.with(|c| *c.borrow_mut() = Some(controller));
    appkit::show_window(&window, mtm);
}

// --- Password prompt (mount an S3 connection whose secret isn't stored) -------

thread_local! {
    static CURRENT_PW: RefCell<Option<Retained<PasswordController>>> = const { RefCell::new(None) };
}

struct PasswordIvars {
    window: Retained<NSWindow>,
    secret: Retained<NSSecureTextField>,
    save_keychain: Retained<NSButton>,
    status: Retained<NSTextField>,
    connection: Connection,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FskitS3PasswordController"]
    #[ivars = PasswordIvars]
    struct PasswordController;

    impl PasswordController {
        #[unsafe(method(submit:))]
        fn submit(&self, _sender: Option<&AnyObject>) {
            self.on_submit();
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            appkit::close_window(&self.ivars().window);
        }
    }

    unsafe impl NSObjectProtocol for PasswordController {}
);

impl PasswordController {
    fn new(mtm: MainThreadMarker, ivars: PasswordIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: `this` is a fresh alloc()+set_ivars, not yet initialized — the precondition of NSObject's designated `-init`.
        unsafe { msg_send![super(this), init] }
    }

    fn on_submit(&self) {
        let iv = self.ivars();
        let secret = appkit::field_string(&iv.secret);
        if secret.is_empty() {
            appkit::set_string(&iv.status, "Secret is required.");
            return;
        }
        if appkit::checkbox_on(&iv.save_keychain) {
            // Best-effort — a mount can still proceed via `-o` even if this fails.
            let _ = crate::keychain::store_secret(&iv.connection.name, &secret);
        }
        let mount_point = iv.connection.default_mount_point();
        if let Err(e) = crate::mounts::mount(&iv.connection, &mount_point, Some(&secret)) {
            appkit::set_string(&iv.status, &format!("Mount failed: {e}"));
            return;
        }
        appkit::close_window(&iv.window);
    }
}

/// Prompt for an S3 connection's secret, then mount it (storing the secret in the
/// Keychain if requested). Used when a mount is requested but no secret is stored.
pub fn open_password(mtm: MainThreadMarker, connection: Connection) {
    let title = format!("Secret for {}", connection.name);
    let window = appkit::make_window(mtm, 360.0, 170.0, &title);
    let Some(content) = appkit::content_view(&window) else {
        return;
    };
    appkit::add_subview(
        &content,
        &appkit::label(
            mtm,
            appkit::rect(20.0, 128.0, 320.0, 20.0),
            "Enter the S3 secret access key:",
        ),
    );
    let secret = appkit::secure_field(mtm, appkit::rect(20.0, 100.0, 320.0, 22.0));
    appkit::add_subview(&content, &secret);
    let save_keychain = appkit::checkbox(
        mtm,
        appkit::rect(20.0, 72.0, 320.0, 20.0),
        "Save to Keychain",
    );
    appkit::add_subview(&content, &save_keychain);
    let status = appkit::label(mtm, appkit::rect(20.0, 46.0, 320.0, 20.0), "");
    appkit::add_subview(&content, &status);
    let cancel = appkit::push_button(mtm, appkit::rect(130.0, 12.0, 100.0, 32.0), "Cancel");
    appkit::add_subview(&content, &cancel);
    let ok = appkit::push_button(mtm, appkit::rect(236.0, 12.0, 104.0, 32.0), "Mount");
    appkit::add_subview(&content, &ok);

    let controller = PasswordController::new(
        mtm,
        PasswordIvars {
            window: window.clone(),
            secret: secret.clone(),
            save_keychain: save_keychain.clone(),
            status: status.clone(),
            connection,
        },
    );
    let target: &AnyObject = &controller;
    appkit::set_target_action(&ok, sel!(submit:), target);
    appkit::set_target_action(&cancel, sel!(cancel:), target);

    CURRENT_PW.with(|c| *c.borrow_mut() = Some(controller));
    appkit::show_window(&window, mtm);
}
