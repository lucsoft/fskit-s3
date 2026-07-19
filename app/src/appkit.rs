//! Checked wrappers over the AppKit calls the app needs.
//!
//! Everything this module exports is safe to call: each wrapper validates its
//! inputs and only performs FFI whose preconditions are met here, so the UI code
//! calls safe Rust. The only `unsafe` elsewhere is in the `define_class!`
//! controllers (main.rs, addwindow.rs): the DSL that *declares* each ObjC class,
//! and the superclass `-init` in their `new` — all documented with `SAFETY:`.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{msg_send, sel, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSAttributedStringAttachmentConveniences,
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor, NSControl,
    NSControlStateValueOff, NSControlStateValueOn, NSImage, NSImageSymbolConfiguration,
    NSLineBreakMode, NSMenu, NSMenuDelegate, NSMenuItem, NSPopUpButton, NSSecureTextField,
    NSStatusItem, NSSwitch, NSTextAlignment, NSTextAttachment, NSTextField, NSTitlePosition,
    NSView, NSWindow, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSAttributedString, NSMutableAttributedString, NSPoint, NSRect, NSSize, NSString, NSURL,
};

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

/// Attach a submenu to a menu item, turning that row into an expandable dropdown.
/// The item's own action (if any) is ignored once it has a submenu.
pub fn set_submenu(item: &NSMenuItem, submenu: &NSMenu) {
    item.setSubmenu(Some(submenu));
}

// --- SF Symbols -------------------------------------------------------------

/// A semantic tint for a status SF Symbol. `None` leaves the symbol untinted
/// (monochrome / template), which is what the menu-bar glyph wants.
#[derive(Clone, Copy)]
pub enum Tint {
    None,
    Green,
    Orange,
    Red,
    Yellow,
    /// The system secondary-label grey — an inactive/"off" state (e.g. unmounted).
    Secondary,
}

impl Tint {
    fn color(self) -> Option<Retained<NSColor>> {
        Some(match self {
            Tint::None => return None,
            Tint::Green => NSColor::systemGreenColor(),
            Tint::Orange => NSColor::systemOrangeColor(),
            Tint::Red => NSColor::systemRedColor(),
            Tint::Yellow => NSColor::systemYellowColor(),
            Tint::Secondary => NSColor::secondaryLabelColor(),
        })
    }
}

/// Build an SF Symbol as an `NSImage`, tinted per `tint`. Returns `None` if the
/// symbol name isn't available on this macOS (so callers degrade gracefully rather
/// than draw a broken glyph).
pub fn symbol_image(name: &str, tint: Tint) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    )?;
    if let Some(color) = tint.color() {
        // Hierarchical color tints the whole symbol in one hue — right for status dots.
        let cfg = NSImageSymbolConfiguration::configurationWithHierarchicalColor(&color);
        if let Some(configured) = image.imageWithSymbolConfiguration(&cfg) {
            return Some(configured);
        }
    }
    Some(image)
}

/// Show an SF Symbol (instead of a text glyph) in the menu bar for a status item.
/// The image is set as a template so it adopts the menu bar's monochrome look and
/// adapts to light/dark automatically.
pub fn set_status_symbol(item: &NSStatusItem, name: &str, mtm: MainThreadMarker) {
    if let Some(button) = item.button(mtm) {
        if let Some(image) = symbol_image(name, Tint::None) {
            image.setTemplate(true);
            button.setImage(Some(&image));
            // Clear any prior text glyph so only the symbol shows.
            button.setTitle(&NSString::from_str(""));
        }
    }
}

/// Set a menu item's leading image to a tinted SF Symbol (a no-op if the symbol
/// isn't available).
pub fn set_menu_item_symbol(item: &NSMenuItem, name: &str, tint: Tint) {
    if let Some(image) = symbol_image(name, tint) {
        item.setImage(Some(&image));
    }
}

/// Set a label's value to a leading tinted SF Symbol followed by `text` (an inline
/// symbol via a text attachment). Falls back to plain text if the symbol is
/// unavailable.
pub fn set_symbol_line(field: &NSTextField, name: &str, tint: Tint, text: &str) {
    let line = NSMutableAttributedString::new();
    if let Some(image) = symbol_image(name, tint) {
        let attachment = NSTextAttachment::new();
        attachment.setImage(Some(&image));
        line.appendAttributedString(&NSAttributedString::attributedStringWithAttachment(
            &attachment,
        ));
        line.appendAttributedString(&plain_attributed("  "));
    }
    line.appendAttributedString(&plain_attributed(text));
    field.setAttributedStringValue(&line);
}

/// A plain (unattributed) attributed string — the text runs of [`set_symbol_line`].
fn plain_attributed(text: &str) -> Retained<NSAttributedString> {
    NSAttributedString::initWithString(NSAttributedString::alloc(), &NSString::from_str(text))
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

/// A rounded, filled "grouped" section — the iOS-Settings / macOS-System-Settings
/// look: a single card that holds a stack of rows. Add rows to its content view
/// ([`box_content`]); hide/show or move the whole card by acting on the box.
pub fn grouped_box(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSBox> {
    // SAFETY: `initWithFrame:` is NSView's designated initializer (inherited by
    // NSBox); a fresh alloc and a valid frame rect are its only preconditions.
    let b: Retained<NSBox> = unsafe { msg_send![NSBox::alloc(mtm), initWithFrame: frame] };
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setBorderWidth(0.0);
    b.setCornerRadius(10.0);
    // A semantic content color so the card stays a touch elevated from the window
    // background and adapts to light/dark automatically.
    b.setFillColor(&NSColor::controlBackgroundColor());
    // Zero margins so the content view fills the card and row frames are exact.
    b.setContentViewMargins(NSSize::new(0.0, 0.0));
    b
}

/// The content view of a [`grouped_box`] — the parent for its rows (origin at the
/// card's bottom-left, sized to the card).
pub fn box_content(b: &NSBox) -> Option<Retained<NSView>> {
    b.contentView()
}

/// A hairline row separator (a horizontal `NSBox` line), used between the rows of
/// a grouped section. Draws in the system separator color and adapts to the theme.
pub fn hairline(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSBox> {
    // SAFETY: same inherited `initWithFrame:` initializer as `grouped_box`.
    let b: Retained<NSBox> = unsafe { msg_send![NSBox::alloc(mtm), initWithFrame: frame] };
    b.setBoxType(NSBoxType::Separator);
    b
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

/// A multi-line label that word-wraps to its frame width instead of clipping.
/// Used for status/validation messages whose length isn't known up front.
pub fn wrapping_label(mtm: MainThreadMarker, frame: NSRect, text: &str) -> Retained<NSTextField> {
    let f = label(mtm, frame, text);
    // `labelWithString:` returns a single-line label; opt into wrapping so long
    // messages flow onto additional lines within the frame.
    f.setUsesSingleLineMode(false);
    f.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    f.setMaximumNumberOfLines(0);
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

/// The value column of a grouped-section row: a borderless, right-aligned field
/// that blends into the card's fill, with greyed `placeholder` text when empty.
pub fn row_field(
    mtm: MainThreadMarker,
    frame: NSRect,
    initial: &str,
    placeholder: &str,
) -> Retained<NSTextField> {
    let f = text_field(mtm, frame, initial);
    style_row_field(&f, placeholder);
    f
}

/// The secure (bulleted) variant of [`row_field`] — the Secret row's value column.
pub fn row_secure_field(
    mtm: MainThreadMarker,
    frame: NSRect,
    initial: &str,
    placeholder: &str,
) -> Retained<NSSecureTextField> {
    let f = secure_field(mtm, frame);
    f.setStringValue(&NSString::from_str(initial));
    style_row_field(&f, placeholder);
    f
}

/// Shared styling for the grouped-row value fields (borderless, no background,
/// right-aligned, placeholder). Takes `&NSTextField`; `NSSecureTextField` is one.
fn style_row_field(f: &NSTextField, placeholder: &str) {
    f.setBezeled(false);
    f.setDrawsBackground(false);
    f.setAlignment(NSTextAlignment::Right);
    f.setPlaceholderString(Some(&NSString::from_str(placeholder)));
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

/// Select the pop-up's item at `index` (out-of-range indices are ignored by AppKit).
pub fn select_popup_index(popup: &NSPopUpButton, index: isize) {
    popup.selectItemAtIndex(index);
}

/// Make a text field editable or not (used to lock the name when editing an
/// existing connection — its name is the stable key the UI addresses).
pub fn set_editable(field: &NSTextField, editable: bool) {
    field.setEditable(editable);
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

/// A modern toggle switch for a grouped-section row, pre-set to `on` (used for the
/// "Save to Keychain" and "Mount when launching" rows).
pub fn make_switch(mtm: MainThreadMarker, frame: NSRect, on: bool) -> Retained<NSSwitch> {
    // SAFETY: `initWithFrame:` is NSView's designated initializer (inherited by
    // NSSwitch); a fresh alloc and a valid frame rect are its only preconditions.
    let s: Retained<NSSwitch> = unsafe { msg_send![NSSwitch::alloc(mtm), initWithFrame: frame] };
    s.setState(if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    s
}

/// Whether a switch is on.
pub fn switch_on(switch: &NSSwitch) -> bool {
    switch.state() == NSControlStateValueOn
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

/// Make a button the window's default (primary): tinted, and triggered by Return.
pub fn set_default_button(button: &NSButton) {
    button.setKeyEquivalent(&NSString::from_str("\r"));
}

/// Mark a button as destructive (the Delete action): the standard macOS destructive
/// role plus a *translucent* red bezel — a subtle tint, not a solid red slab.
pub fn set_button_destructive(button: &NSButton) {
    button.setHasDestructiveAction(true);
    let red = NSColor::systemRedColor().colorWithAlphaComponent(0.5);
    button.setBezelColor(Some(&red));
}

/// Run `work` on a background thread, then message `selector` on `target` back on
/// the **main** thread (via `performSelectorOnMainThread:`). The pattern for a
/// latency-bound call (a network/XPC round-trip) that must not block the UI: do
/// the blocking work off-main, stash its result somewhere the selector can read,
/// and let the selector apply it on the main thread.
///
/// Only a raw pointer + selector cross the thread boundary, and they're used
/// solely to enqueue a main-thread message — never to touch the object's Rust
/// state off-main. `performSelectorOnMainThread:` retains `target` until the
/// selector runs, so it can't be freed underneath the pending message.
pub fn run_off_main_then_notify<F>(target: &AnyObject, selector: Sel, work: F)
where
    F: FnOnce() + Send + 'static,
{
    struct Hop {
        target: *const AnyObject,
        selector: Sel,
    }
    // SAFETY: the pointer is only ever used to send a thread-safe ObjC message
    // (`performSelectorOnMainThread:`), never to access the object off the main
    // thread; the selector is a process-global immutable value.
    unsafe impl Send for Hop {}

    let hop = Hop {
        target: target as *const AnyObject,
        selector,
    };
    std::thread::spawn(move || {
        // Bind the whole struct so the closure captures `Hop` (which is `Send`),
        // not its raw-pointer field on its own (edition-2021 disjoint capture).
        let hop = hop;
        work();
        // SAFETY: `performSelectorOnMainThread:withObject:waitUntilDone:` is safe to
        // call from any thread; it schedules the selector on the main run loop and
        // retains the receiver until it runs. `hop.target` was a live object when
        // this call was made and stays valid for the app's lifetime.
        unsafe {
            if let Some(obj) = (hop.target as *mut AnyObject).as_ref() {
                let _: () = msg_send![
                    obj,
                    performSelectorOnMainThread: hop.selector,
                    withObject: core::ptr::null_mut::<AnyObject>(),
                    waitUntilDone: false,
                ];
            }
        }
    });
}

/// Open **System Settings ▸ General ▸ Login Items & Extensions ▸ File System
/// Extensions** — where the user enables the extension (macOS won't let the app
/// flip that toggle itself). Best-effort: a bad URL or refused open is ignored.
pub fn open_extensions_settings() {
    let url = NSURL::URLWithString(&NSString::from_str(
        "x-apple.systempreferences:com.apple.ExtensionsPreferences",
    ));
    if let Some(url) = url {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

/// Show a modal error alert with a single OK button.
pub fn show_error(mtm: MainThreadMarker, message: &str, info: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(info));
    alert.addButtonWithTitle(&NSString::from_str("OK"));
    alert.runModal();
}

/// Show a modal "are you sure?" alert with a destructive default button. Returns
/// `true` only if the user confirms (clicks `confirm_title`).
pub fn confirm(mtm: MainThreadMarker, message: &str, info: &str, confirm_title: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(info));
    // First button is the default; add the destructive action first, then Cancel.
    alert.addButtonWithTitle(&NSString::from_str(confirm_title));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.runModal() == NSAlertFirstButtonReturn
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
/// Animates (the standard macOS resize) only when the window is already on screen,
/// so the initial pre-`show` sizing is instant.
pub fn set_window_content_size(window: &NSWindow, w: f64, h: f64) {
    let frame_size = window.frameRectForContentRect(rect(0.0, 0.0, w, h)).size;
    let old = window.frame();
    let top = old.origin.y + old.size.height;
    let new_frame = NSRect::new(
        NSPoint::new(old.origin.x, top - frame_size.height),
        frame_size,
    );
    window.setFrame_display_animate(new_frame, true, window.isVisible());
}
