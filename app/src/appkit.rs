//! Checked wrappers over the AppKit calls the app needs.
//!
//! Everything this module exports is safe to call: each wrapper validates its
//! inputs and only performs FFI whose preconditions are met here, so the UI code
//! calls safe Rust. The only `unsafe` elsewhere is in the `define_class!`
//! controllers (main.rs, addwindow.rs): the DSL that *declares* each ObjC class,
//! and the superclass `-init` in their `new` — all documented with `SAFETY:`.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSButton, NSControl,
    NSControlStateValueOn, NSMenu, NSMenuDelegate, NSMenuItem, NSPopUpButton, NSSecureTextField,
    NSStatusItem, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// A fresh, empty menu.
pub fn menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    NSMenu::new(mtm)
}

/// Make `delegate` the menu's delegate. Its `menuNeedsUpdate:` is then called
/// before every display, so the caller can rebuild the menu on demand.
///
/// The menu holds the delegate weakly, so the caller must keep it alive for as
/// long as the menu exists (the controller does — it lives for the whole run).
pub fn set_menu_delegate(menu: &NSMenu, delegate: &ProtocolObject<dyn NSMenuDelegate>) {
    menu.setDelegate(Some(delegate));
}

/// Remove every item from a menu (called before repopulating it).
pub fn clear_menu(menu: &NSMenu) {
    menu.removeAllItems();
}

/// A separator menu item.
pub fn separator(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    NSMenuItem::separatorItem(mtm)
}

/// Build a menu item.
///
/// `action`/`target` wire the click (target must outlive the menu — the
/// controller does). `represented` stashes a string the handler reads back with
/// [`represented_string`]. `enabled` greys the row out when `false`.
pub fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    target: Option<&AnyObject>,
    represented: Option<&str>,
    enabled: bool,
) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    if let Some(sel) = action {
        // SAFETY: `sel` is a compile-time selector literal; setAction just stores it.
        unsafe { item.setAction(Some(sel)) };
    }
    if let Some(target) = target {
        // SAFETY: `target` is a live object reference; setTarget only retains it.
        unsafe { item.setTarget(Some(target)) };
    }
    if let Some(value) = represented {
        // SAFETY: we store an owned NSString; only our own code reads it back.
        unsafe { item.setRepresentedObject(Some(&NSString::from_str(value))) };
    }
    item.setEnabled(enabled);
    item
}

/// Read the string stashed by [`menu_item`]'s `represented`, if present and a string.
pub fn represented_string(item: &NSMenuItem) -> Option<String> {
    let obj = item.representedObject()?;
    obj.downcast::<NSString>().ok().map(|s| s.to_string())
}

/// Attach a menu to a status item.
pub fn set_menu(item: &NSStatusItem, menu: &NSMenu) {
    item.setMenu(Some(menu));
}

/// Set the glyph shown in the menu bar for a status item.
pub fn set_status_title(item: &NSStatusItem, title: &str, mtm: MainThreadMarker) {
    if let Some(button) = item.button(mtm) {
        button.setTitle(&NSString::from_str(title));
    }
}

/// Install a main menu with an **Edit** submenu (Cut/Copy/Paste/Select All) so
/// standard editing shortcuts (⌘X/C/V/A) reach the focused text field.
///
/// Without a main menu, an `Accessory` app never routes those key equivalents to
/// the first responder — so text fields can't be pasted into. The items target
/// `nil` (the first responder), whose field editor implements the selectors. The
/// menu bar isn't shown for an accessory app, but its key equivalents still fire.
pub fn install_edit_menu(mtm: MainThreadMarker) {
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Edit"));
    edit.addItem(&responder_item(mtm, "Cut", sel!(cut:), "x"));
    edit.addItem(&responder_item(mtm, "Copy", sel!(copy:), "c"));
    edit.addItem(&responder_item(mtm, "Paste", sel!(paste:), "v"));
    edit.addItem(&responder_item(mtm, "Select All", sel!(selectAll:), "a"));

    let edit_item = NSMenuItem::new(mtm);
    edit_item.setSubmenu(Some(&edit));

    let main = NSMenu::new(mtm);
    main.addItem(&edit_item);
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
}

/// A menu item bound to `action` with a ⌘-`key` equivalent, targeting the first
/// responder (target stays nil) — the pattern for standard editing selectors.
fn responder_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    key: &str,
) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    // SAFETY: `action` is a compile-time selector; leaving the target nil routes it
    // up the responder chain (NSMenuItem's default modifier is ⌘).
    unsafe { item.setAction(Some(action)) };
    item.setKeyEquivalent(&NSString::from_str(key));
    item
}

// --- Window + form controls (for the Add-mount window) ---------------------

/// An `NSRect` from origin + size, in AppKit's bottom-left coordinate space.
pub fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// A titled, closable window of the given content size, centered and *not*
/// released when closed (the controller keeps ownership).
pub fn make_window(mtm: MainThreadMarker, w: f64, h: f64, title: &str) -> Retained<NSWindow> {
    // SAFETY: standard NSWindow designated initializer — valid content rect, style
    // mask, and backing store; defer:false builds the window device now.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, w, h),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str(title));
    // SAFETY: the controller retains the window for its whole lifetime, so it must
    // not auto-release when closed (the AppKit default for controller-less windows).
    unsafe { window.setReleasedWhenClosed(false) };
    window.center();
    window
}

/// The window's content view — the parent for form controls.
pub fn content_view(window: &NSWindow) -> Option<Retained<NSView>> {
    window.contentView()
}

/// A plain container view (used to group + toggle the S3 fields at once).
pub fn plain_view(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    NSView::initWithFrame(NSView::alloc(mtm), frame)
}

/// Add a control as a subview of `parent`.
pub fn add_subview(parent: &NSView, child: &NSView) {
    parent.addSubview(child);
}

/// Bring a window to the front, make it key, and activate the app (needed for an
/// Accessory app so its text fields accept keyboard input).
pub fn show_window(window: &NSWindow, mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    // `activate` (no args) is the modern replacement but only macOS 14+; the
    // deprecated form is universally available and correct here.
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    window.makeKeyAndOrderFront(None);
}

/// Close (order out) a window.
pub fn close_window(window: &NSWindow) {
    window.close();
}

/// A non-editable label.
pub fn label(mtm: MainThreadMarker, frame: NSRect, text: &str) -> Retained<NSTextField> {
    let f = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    f.setFrame(frame);
    f
}

/// An editable single-line text field, pre-filled with `initial`.
pub fn text_field(mtm: MainThreadMarker, frame: NSRect, initial: &str) -> Retained<NSTextField> {
    let f = NSTextField::new(mtm);
    f.setStringValue(&NSString::from_str(initial));
    f.setFrame(frame);
    f
}

/// An editable single-line password field (bullets).
pub fn secure_field(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSSecureTextField> {
    let f = NSSecureTextField::new(mtm);
    f.setFrame(frame);
    f
}

/// A pop-up button pre-populated with `items`. Wire its action later with
/// [`set_target_action`] once the target object exists.
pub fn popup(mtm: MainThreadMarker, frame: NSRect, items: &[&str]) -> Retained<NSPopUpButton> {
    let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame, false);
    for item in items {
        p.addItemWithTitle(&NSString::from_str(item));
    }
    p
}

/// Wire a control's target + action (the target must outlive the control).
pub fn set_target_action(control: &NSControl, action: Sel, target: &AnyObject) {
    // SAFETY: `action` is a compile-time selector; `target` is a live object that
    // the caller keeps alive for the control's lifetime.
    unsafe {
        control.setAction(Some(action));
        control.setTarget(Some(target));
    }
}

/// The index of the pop-up's selected item (-1 if none).
pub fn popup_index(popup: &NSPopUpButton) -> isize {
    popup.indexOfSelectedItem()
}

/// A checkbox (no action — its state is read on save).
pub fn checkbox(mtm: MainThreadMarker, frame: NSRect, title: &str) -> Retained<NSButton> {
    // SAFETY: no target/action is wired (target=None, action=None), so there are
    // no selector/target validity requirements to uphold.
    let b = unsafe {
        NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    b.setFrame(frame);
    b
}

/// Whether a checkbox is checked.
pub fn checkbox_on(button: &NSButton) -> bool {
    button.state() == NSControlStateValueOn
}

/// A push button. Wire its action later with [`set_target_action`].
pub fn push_button(mtm: MainThreadMarker, frame: NSRect, title: &str) -> Retained<NSButton> {
    // SAFETY: no target/action wired here (both None), so nothing to uphold.
    let b = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    b.setFrame(frame);
    b
}

/// Read a text field's current string.
pub fn field_string(field: &NSTextField) -> String {
    field.stringValue().to_string()
}

/// Set a label/field's displayed string (used for the inline status line).
pub fn set_string(field: &NSTextField, text: &str) {
    field.setStringValue(&NSString::from_str(text));
}

/// Show or hide a control.
pub fn set_hidden(view: &NSView, hidden: bool) {
    view.setHidden(hidden);
}

/// Pin a control to the top edge of its superview (fixed distance from the top;
/// the space below flexes when the window resizes).
pub fn pin_top(view: &NSView) {
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
}

/// Pin a control to the bottom edge of its superview (fixed distance from the
/// bottom; the space above flexes).
pub fn pin_bottom(view: &NSView) {
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
}

/// Resize a window so its content area is `w × h`, keeping the **top** edge fixed
/// (it grows/shrinks downward). Subviews reposition per their autoresizing masks.
pub fn set_window_content_size(window: &NSWindow, w: f64, h: f64) {
    let frame_size = window.frameRectForContentRect(rect(0.0, 0.0, w, h)).size;
    let old = window.frame();
    let top = old.origin.y + old.size.height;
    let new_frame = NSRect::new(
        NSPoint::new(old.origin.x, top - frame_size.height),
        frame_size,
    );
    window.setFrame_display(new_frame, true);
}
