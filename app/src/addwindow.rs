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
use objc2_app_kit::{
    NSBox, NSButton, NSPopUpButton, NSSecureTextField, NSSwitch, NSTextField, NSView, NSWindow,
};

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
    s3_box: Retained<NSBox>,
    endpoint: Retained<NSTextField>,
    bucket: Retained<NSTextField>,
    region: Retained<NSTextField>,
    akid: Retained<NSTextField>,
    secret: Retained<NSSecureTextField>,
    token: Retained<NSTextField>,
    save_keychain: Retained<NSSwitch>,
    mount_launch: Retained<NSSwitch>,
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
            save_secret_to_keychain: appkit::switch_on(&iv.save_keychain),
            mount_on_launch: appkit::switch_on(&iv.mount_launch),
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

/// Grouped-section geometry (an iOS-Settings-style card of stacked rows).
/// Cards are inset [`MARGIN`] from the window edges; rows are [`ROW_H`] tall with
/// the label at [`ROW_INSET`] from the left and a right-aligned value column.
const WINDOW_W: f64 = 400.0;
const MARGIN: f64 = 20.0;
const GROUP_W: f64 = WINDOW_W - 2.0 * MARGIN;
const GAP: f64 = 16.0;
const ROW_H: f64 = 40.0;
const ROW_INSET: f64 = 16.0;
const LABEL_W: f64 = 120.0;
const VALUE_X: f64 = ROW_INSET + LABEL_W + 8.0;
const VALUE_W: f64 = GROUP_W - VALUE_X - ROW_INSET;
const STATUS_H: f64 = 40.0;
const BUTTON_H: f64 = 32.0;
/// Rows in each section: identity (Name, Type); S3 (Endpoint, Bucket, Region,
/// Access Key ID, Secret, Session token, Save to Keychain); options (Mount).
const ID_ROWS: usize = 2;
const S3_ROWS: usize = 7;
const OPT_ROWS: usize = 1;
const ID_H: f64 = ID_ROWS as f64 * ROW_H;
const S3_H: f64 = S3_ROWS as f64 * ROW_H;
const OPT_H: f64 = OPT_ROWS as f64 * ROW_H;

// Card / control y-origins (bottom-left) at the tall S3 height. Identity + S3 pin
// to the top; the options card, status line, and buttons pin to the bottom above
// the buttons, so hiding the S3 card and shrinking the window collapses the middle.
const BUTTONS_Y: f64 = MARGIN;
// A full-width divider separating the form content from the action-button row.
const SEP_Y: f64 = BUTTONS_Y + BUTTON_H + 12.0;
const STATUS_Y: f64 = SEP_Y + 12.0;
const OPT_Y: f64 = STATUS_Y + STATUS_H + GAP;
// The two heights the form toggles between: the difference is exactly the S3 card
// plus one inter-section gap, so shrinking to the memory height removes the S3 card
// cleanly (see `update_visibility`).
const WINDOW_H_S3: f64 = OPT_Y + OPT_H + GAP + S3_H + GAP + ID_H + MARGIN;
const WINDOW_H_MEMORY: f64 = WINDOW_H_S3 - S3_H - GAP;
const ID_Y: f64 = WINDOW_H_S3 - MARGIN - ID_H;
const S3_Y: f64 = ID_Y - GAP - S3_H;

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
    // The identity + S3 cards track the top edge; the options card, status, and
    // buttons track the bottom edge, so hiding the S3 card and shrinking the window
    // (see `update_visibility`) collapses the gap cleanly.
    let add_top = |v: &NSView| {
        appkit::pin_top(v);
        appkit::add_subview(&content, v);
    };
    let add_bottom = |v: &NSView| {
        appkit::pin_bottom(v);
        appkit::add_subview(&content, v);
    };

    // A value row inside a grouped card `cv` of `rows` rows: adds the left label
    // (and a hairline above every row but the first) and returns the frame the
    // caller drops the right-aligned value control into. Coordinates are local to
    // the card's content view (bottom-left origin, height = rows * ROW_H).
    let row = |cv: &NSView, rows: usize, idx: usize, text: &str| -> objc2_foundation::NSRect {
        let card_h = rows as f64 * ROW_H;
        let top = card_h - idx as f64 * ROW_H;
        let center = top - ROW_H / 2.0;
        appkit::add_subview(
            cv,
            &appkit::label(
                mtm,
                appkit::rect(ROW_INSET, center - 10.0, LABEL_W, 20.0),
                text,
            ),
        );
        if idx > 0 {
            appkit::add_subview(
                cv,
                &appkit::hairline(
                    mtm,
                    appkit::rect(ROW_INSET, top - 0.5, GROUP_W - ROW_INSET, 1.0),
                ),
            );
        }
        appkit::rect(VALUE_X, center - 11.0, VALUE_W, 22.0)
    };

    // A toggle row: a wide label on the left and a switch on the right. Returns the
    // switch so the caller can stash it (its state is read on save).
    let toggle_row = |cv: &NSView, rows: usize, idx: usize, text: &str, on: bool| {
        const SWITCH_W: f64 = 38.0;
        let card_h = rows as f64 * ROW_H;
        let top = card_h - idx as f64 * ROW_H;
        let center = top - ROW_H / 2.0;
        let label_w = GROUP_W - ROW_INSET - SWITCH_W - 16.0;
        appkit::add_subview(
            cv,
            &appkit::label(
                mtm,
                appkit::rect(ROW_INSET, center - 10.0, label_w, 20.0),
                text,
            ),
        );
        if idx > 0 {
            appkit::add_subview(
                cv,
                &appkit::hairline(
                    mtm,
                    appkit::rect(ROW_INSET, top - 0.5, GROUP_W - ROW_INSET, 1.0),
                ),
            );
        }
        let s = appkit::make_switch(
            mtm,
            appkit::rect(
                GROUP_W - ROW_INSET - SWITCH_W,
                center - 10.5,
                SWITCH_W,
                21.0,
            ),
            on,
        );
        appkit::add_subview(cv, &s);
        s
    };

    // --- Identity card (Name, Type) — pinned to the top. ---
    let id_box = appkit::grouped_box(mtm, appkit::rect(MARGIN, ID_Y, GROUP_W, ID_H));
    add_top(&id_box);
    let Some(id_cv) = appkit::box_content(&id_box) else {
        return;
    };
    // Name (locked when editing — it's the registry key).
    let name = appkit::row_field(mtm, row(&id_cv, ID_ROWS, 0, "Name"), &init_name, "Required");
    if original_name.is_some() {
        appkit::set_editable(&name, false);
    }
    appkit::add_subview(&id_cv, &name);
    // Type selector.
    let type_rect = row(&id_cv, ID_ROWS, 1, "Type");
    let type_popup = appkit::popup(
        mtm,
        appkit::rect(
            type_rect.origin.x,
            type_rect.origin.y - 1.5,
            type_rect.size.width,
            25.0,
        ),
        &["In-memory", "S3"],
    );
    appkit::select_popup_index(&type_popup, if is_s3_init { 1 } else { 0 });
    appkit::add_subview(&id_cv, &type_popup);

    // --- S3 card — pinned to the top under identity; toggled with the type. ---
    let s3_box = appkit::grouped_box(mtm, appkit::rect(MARGIN, S3_Y, GROUP_W, S3_H));
    add_top(&s3_box);
    let Some(s3_cv) = appkit::box_content(&s3_box) else {
        return;
    };
    let endpoint = appkit::row_field(
        mtm,
        row(&s3_cv, S3_ROWS, 0, "Endpoint"),
        &init_endpoint,
        "https://s3.example.com",
    );
    appkit::add_subview(&s3_cv, &endpoint);
    let bucket = appkit::row_field(
        mtm,
        row(&s3_cv, S3_ROWS, 1, "Bucket"),
        &init_bucket,
        "Required",
    );
    appkit::add_subview(&s3_cv, &bucket);
    let region = appkit::row_field(mtm, row(&s3_cv, S3_ROWS, 2, "Region"), &init_region, "auto");
    appkit::add_subview(&s3_cv, &region);
    let akid = appkit::row_field(
        mtm,
        row(&s3_cv, S3_ROWS, 3, "Access Key ID"),
        &init_akid,
        "Required",
    );
    appkit::add_subview(&s3_cv, &akid);
    let secret = appkit::row_secure_field(
        mtm,
        row(&s3_cv, S3_ROWS, 4, "Secret"),
        &init_secret,
        "Required",
    );
    appkit::add_subview(&s3_cv, &secret);
    let token = appkit::row_field(
        mtm,
        row(&s3_cv, S3_ROWS, 5, "Session token"),
        &init_token,
        "Optional",
    );
    appkit::add_subview(&s3_cv, &token);
    let save_keychain = toggle_row(
        &s3_cv,
        S3_ROWS,
        6,
        "Save secret to Keychain",
        init_save_keychain,
    );

    // --- Options card (Mount when launching) — pinned to the bottom. ---
    let opt_box = appkit::grouped_box(mtm, appkit::rect(MARGIN, OPT_Y, GROUP_W, OPT_H));
    add_bottom(&opt_box);
    let Some(opt_cv) = appkit::box_content(&opt_box) else {
        return;
    };
    let mount_launch = toggle_row(
        &opt_cv,
        OPT_ROWS,
        0,
        "Mount when launching",
        init_mount_launch,
    );

    // Status line + a full-width divider + buttons (pinned to the bottom edge).
    let status = appkit::wrapping_label(mtm, appkit::rect(MARGIN, STATUS_Y, GROUP_W, STATUS_H), "");
    add_bottom(&status);
    add_bottom(&appkit::hairline(
        mtm,
        appkit::rect(0.0, SEP_Y, WINDOW_W, 1.0),
    ));
    let save = appkit::push_button(
        mtm,
        appkit::rect(WINDOW_W - MARGIN - 120.0, BUTTONS_Y, 120.0, BUTTON_H),
        "Test & Save",
    );
    appkit::set_default_button(&save); // primary button (tinted, triggered by Return)
    add_bottom(&save);
    let cancel = appkit::push_button(
        mtm,
        appkit::rect(WINDOW_W - MARGIN - 228.0, BUTTONS_Y, 100.0, BUTTON_H),
        "Cancel",
    );
    add_bottom(&cancel);
    // Destructive Delete, only when editing an existing connection (left corner).
    let delete = original_name.is_some().then(|| {
        let b = appkit::push_button(
            mtm,
            appkit::rect(MARGIN, BUTTONS_Y, 100.0, BUTTON_H),
            "Delete",
        );
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
    let window = appkit::make_window(mtm, 360.0, 210.0, &title);
    let Some(content) = appkit::content_view(&window) else {
        return;
    };
    appkit::add_subview(
        &content,
        &appkit::label(
            mtm,
            appkit::rect(20.0, 168.0, 320.0, 20.0),
            "Enter the S3 secret access key:",
        ),
    );
    let secret = appkit::secure_field(mtm, appkit::rect(20.0, 140.0, 320.0, 22.0));
    appkit::add_subview(&content, &secret);
    let save_keychain = appkit::checkbox(
        mtm,
        appkit::rect(20.0, 112.0, 320.0, 20.0),
        "Save to Keychain",
    );
    appkit::add_subview(&content, &save_keychain);
    // Wrapping, so a long "Mount failed: …" error shows in full instead of clipping.
    let status = appkit::wrapping_label(mtm, appkit::rect(20.0, 52.0, 320.0, 54.0), "");
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
