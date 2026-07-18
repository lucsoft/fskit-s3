//! The connection window — create a connection ([`open`]) or edit an existing one
//! ([`open_edit`]), In-memory or S3.
//!
//! A native `NSWindow` form built via [`crate::appkit`] (all FFI stays there).
//! On **Test & Save** an S3 connection's credentials are validated by listing the
//! bucket ([`crate::s3check`]); on success the connection is persisted and, when
//! "Save secret to Keychain" is checked, the secret is stored via
//! [`crate::keychain`]. When editing, the fields are pre-filled (the secret from
//! the Keychain) and the name is locked — it's the registry key, so on save the
//! previous entry is replaced in place; a red **Delete** button (shown only when
//! editing) unmounts the connection if mounted, then removes it and its secret,
//! after a confirmation. The menu picks up the change on its next open (it reloads
//! the registry from disk).

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSButton, NSPopUpButton, NSSecureTextField, NSTextField, NSView, NSWindow};

use crate::appkit;
use crate::connection::{Connection, ConnectionKind, FormInput, Registry};

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
    /// The name of the connection being edited, or `None` when creating a new one.
    /// In edit mode the name field is locked to this, so it doubles as the registry
    /// key to replace on save.
    original_name: Option<String>,
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

        #[unsafe(method(delete:))]
        fn delete(&self, _sender: Option<&AnyObject>) {
            self.on_delete();
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

    /// Validate the form, run the live S3 check, persist, and close on success.
    fn on_save(&self) {
        let iv = self.ivars();
        let set_status = |msg: &str| appkit::set_string(&iv.status, msg);

        let input = FormInput {
            name: appkit::field_string(&iv.name),
            is_s3: appkit::popup_index(&iv.type_popup) == 1,
            endpoint: appkit::field_string(&iv.endpoint),
            bucket: appkit::field_string(&iv.bucket),
            region: appkit::field_string(&iv.region),
            access_key_id: appkit::field_string(&iv.akid),
            secret: appkit::field_string(&iv.secret),
            session_token: appkit::field_string(&iv.token),
            save_secret_to_keychain: appkit::checkbox_on(&iv.save_keychain),
            mount_on_launch: appkit::checkbox_on(&iv.mount_launch),
        };
        // Keep what the live check + Keychain need after `from_form` consumes `input`.
        let secret = input.secret.clone();
        let save_keychain = input.save_secret_to_keychain;

        let conn = match Connection::from_form(input) {
            Ok(conn) => conn,
            Err(e) => {
                set_status(&e);
                return;
            }
        };

        if let ConnectionKind::S3(meta) = &conn.kind {
            // NOTE: synchronous network check — the UI briefly blocks (fine for a
            // local endpoint; a background thread is a later refinement).
            if let Err(e) = crate::s3check::test_s3(meta, &secret) {
                set_status(&format!("Couldn't reach the bucket: {e}"));
                return;
            }
            if save_keychain {
                if let Err(e) = crate::keychain::store_secret(&conn.name, &secret) {
                    set_status(&format!("Keychain save failed: {e}"));
                    return;
                }
            }
        }

        let mut registry = Registry::load();
        match &iv.original_name {
            // Editing: drop the previous entry (the name is locked, so it matches)
            // and re-add the updated one in its place.
            Some(orig) => {
                registry.remove(orig);
            }
            // Creating: the name must be free.
            None => {
                if registry.get(&conn.name).is_some() {
                    set_status(&format!(
                        "A connection named {:?} already exists.",
                        conn.name
                    ));
                    return;
                }
            }
        }
        if let Err(e) = registry.add(conn) {
            set_status(&e);
            return;
        }
        if let Err(e) = registry.save() {
            set_status(&format!("Save failed: {e}"));
            return;
        }
        appkit::close_window(&iv.window);
    }

    /// Delete the connection being edited (after a confirmation): unmount it if
    /// mounted, then drop it from the registry and its secret from the Keychain, and
    /// close. A no-op when creating a new connection (the Delete button isn't shown
    /// then). If the unmount fails (e.g. the volume is busy) the delete is aborted
    /// so we never orphan a live mount whose config is gone.
    fn on_delete(&self) {
        let iv = self.ivars();
        let Some(name) = iv.original_name.clone() else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if !appkit::confirm(
            mtm,
            &format!("Delete the connection “{name}”?"),
            "This unmounts it if mounted, then removes its configuration and stored secret. This can't be undone.",
            "Delete",
        ) {
            return;
        }

        let mut registry = Registry::load();
        // Unmount first if currently mounted at its default point; abort on failure.
        if let Some(conn) = registry.get(&name) {
            let mount_point = conn.default_mount_point();
            let mount_point = mount_point.to_string_lossy();
            let mounted = crate::mounts::list_fskit()
                .iter()
                .any(|m| m.mount_point == *mount_point);
            if mounted {
                if let Err(e) = crate::mounts::unmount(&mount_point) {
                    appkit::set_string(&iv.status, &format!("Couldn't unmount: {e}"));
                    return;
                }
            }
        }

        registry.remove(&name);
        if let Err(e) = registry.save() {
            appkit::set_string(&iv.status, &format!("Delete failed: {e}"));
            return;
        }
        crate::keychain::delete_secret(&name);
        appkit::close_window(&iv.window);
    }
}

/// Window content width, and the two heights the form toggles between (S3 fields
/// shown vs. hidden). The bottom group (checkbox/status/buttons) is pinned to the
/// bottom, so shrinking the window collapses the empty middle.
const WINDOW_W: f64 = 380.0;
const WINDOW_H_S3: f64 = 440.0;
const WINDOW_H_MEMORY: f64 = 240.0;

/// Open the form to create a new connection.
pub fn open(mtm: MainThreadMarker) {
    open_form(mtm, None);
}

/// Open the form pre-filled to edit an existing connection. The name is locked
/// (it's the registry key), and the S3 secret is pre-loaded from the Keychain when
/// available so the live check + re-save work without re-entering it.
pub fn open_edit(mtm: MainThreadMarker, connection: Connection) {
    open_form(mtm, Some(connection));
}

/// Build and show the connection form. With `existing` it edits that connection
/// (fields pre-filled, name locked); without it, it creates a new one. Controls
/// are laid out for the tall (S3) height; `update_visibility` shrinks to the memory
/// height for the In-memory selection before the window is shown.
fn open_form(mtm: MainThreadMarker, existing: Option<Connection>) {
    // Initial field values (empty for a new connection).
    let s3_meta = existing.as_ref().and_then(|c| match &c.kind {
        ConnectionKind::S3(m) => Some(m.clone()),
        ConnectionKind::Memory => None,
    });
    let init_name = existing
        .as_ref()
        .map(|c| c.name.clone())
        .unwrap_or_default();
    let is_s3_init = s3_meta.is_some();
    let init = |f: fn(&crate::connection::S3Meta) -> &str| {
        s3_meta.as_ref().map(f).unwrap_or("").to_string()
    };
    let init_endpoint = init(|m| &m.endpoint);
    let init_bucket = init(|m| &m.bucket);
    let init_region = init(|m| &m.region);
    let init_akid = init(|m| &m.access_key_id);
    let init_token = s3_meta
        .as_ref()
        .and_then(|m| m.session_token.clone())
        .unwrap_or_default();
    // Pre-load the stored secret so editing an S3 connection needn't re-type it.
    let init_secret = existing
        .as_ref()
        .filter(|c| c.is_s3())
        .and_then(|c| crate::keychain::read_secret(&c.name))
        .unwrap_or_default();
    let init_save_keychain = existing
        .as_ref()
        .map(|c| c.save_secret_to_keychain)
        .unwrap_or(false);
    let init_mount_launch = existing
        .as_ref()
        .map(|c| c.mount_on_launch)
        .unwrap_or(false);
    let original_name = existing.as_ref().map(|c| c.name.clone());

    let title = if existing.is_some() {
        "Edit Connection"
    } else {
        "New Connection"
    };
    let window = appkit::make_window(mtm, WINDOW_W, WINDOW_H_S3, title);
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

    // Name (locked when editing — it's the registry key).
    add_top(&appkit::label(
        mtm,
        appkit::rect(20.0, 400.0, 120.0, 20.0),
        "Name",
    ));
    let name = appkit::text_field(mtm, appkit::rect(150.0, 398.0, 210.0, 22.0), &init_name);
    if original_name.is_some() {
        appkit::set_editable(&name, false);
    }
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
    appkit::select_popup_index(&type_popup, if is_s3_init { 1 } else { 0 });
    add_top(&type_popup);

    // S3 fields, grouped in a box so the whole group toggles + moves at once.
    let s3_box = appkit::plain_view(mtm, appkit::rect(0.0, 150.0, 380.0, 205.0));
    let field_row = |label_text: &str, rel_y: f64, initial: &str| -> Retained<NSTextField> {
        appkit::add_subview(
            &s3_box,
            &appkit::label(
                mtm,
                appkit::rect(20.0, rel_y + 2.0, 120.0, 20.0),
                label_text,
            ),
        );
        let f = appkit::text_field(mtm, appkit::rect(150.0, rel_y, 210.0, 22.0), initial);
        appkit::add_subview(&s3_box, &f);
        f
    };
    let endpoint = field_row("Endpoint", 178.0, &init_endpoint);
    let bucket = field_row("Bucket", 150.0, &init_bucket);
    let region = field_row("Region", 122.0, &init_region);
    let akid = field_row("Access Key ID", 94.0, &init_akid);
    appkit::add_subview(
        &s3_box,
        &appkit::label(mtm, appkit::rect(20.0, 68.0, 120.0, 20.0), "Secret"),
    );
    let secret = appkit::secure_field(mtm, appkit::rect(150.0, 66.0, 210.0, 22.0));
    appkit::set_string(&secret, &init_secret);
    appkit::add_subview(&s3_box, &secret);
    let token = field_row("Session token", 38.0, &init_token);
    let save_keychain = appkit::checkbox(
        mtm,
        appkit::rect(150.0, 8.0, 210.0, 20.0),
        "Save secret to Keychain",
    );
    appkit::set_checkbox_on(&save_keychain, init_save_keychain);
    appkit::add_subview(&s3_box, &save_keychain);
    add_top(&s3_box);

    // Always-visible options + status + buttons (pinned to the bottom edge).
    let mount_launch = appkit::checkbox(
        mtm,
        appkit::rect(150.0, 100.0, 210.0, 20.0),
        "Mount when launching",
    );
    appkit::set_checkbox_on(&mount_launch, init_mount_launch);
    add_bottom(&mount_launch);
    let status = appkit::wrapping_label(mtm, appkit::rect(20.0, 56.0, 340.0, 44.0), "");
    add_bottom(&status);
    let cancel = appkit::push_button(mtm, appkit::rect(150.0, 14.0, 100.0, 32.0), "Cancel");
    add_bottom(&cancel);
    let save = appkit::push_button(mtm, appkit::rect(256.0, 14.0, 104.0, 32.0), "Test & Save");
    appkit::set_default_button(&save); // primary button (tinted, triggered by Return)
    add_bottom(&save);
    // Destructive Delete, only when editing an existing connection (left corner).
    let delete = original_name.is_some().then(|| {
        let b = appkit::push_button(mtm, appkit::rect(20.0, 14.0, 100.0, 32.0), "Delete");
        appkit::set_button_destructive(&b);
        add_bottom(&b);
        b
    });

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
            original_name,
        },
    );

    // Wire actions now that the target exists (same underlying objects as ivars).
    let target: &AnyObject = &controller;
    appkit::set_target_action(&type_popup, sel!(typeChanged:), target);
    appkit::set_target_action(&save, sel!(testAndSave:), target);
    appkit::set_target_action(&cancel, sel!(cancel:), target);
    if let Some(delete) = &delete {
        appkit::set_target_action(delete, sel!(delete:), target);
    }

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
