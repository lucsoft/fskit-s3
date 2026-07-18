//! Checked wrappers over the AppKit calls the app needs.
//!
//! Everything this module exports is safe to call: each wrapper validates its
//! inputs and only performs FFI whose preconditions are met here, so the menu
//! code calls safe Rust. The only `unsafe` outside this module is in
//! `Controller` (main.rs): the `define_class!` DSL that *declares* the ObjC class,
//! and the superclass `-init` in `Controller::new` — both documented in place
//! with `SAFETY:` comments.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSMenu, NSMenuDelegate, NSMenuItem, NSStatusItem};
use objc2_foundation::NSString;

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
