//! The extension-health window — what the old standalone host app's window did,
//! now raised from the menu-bar app.
//!
//! It shows whether the FSKit extension is installed + enabled (and flags a build
//! mismatch), plus the launch-at-login state. macOS won't let the app enable a
//! file-system extension itself, so the primary action deep-links to the right
//! System Settings pane. It's raised automatically at launch when the extension
//! isn't ready, and on demand from the menu's health row.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSTextField, NSWindow};

use crate::appkit::{self, Tint};
use crate::autostart;
use crate::health::{self, Freshness, Health, Report};

// Keeps the live window controller retained while the window is open (main-thread
// only). Replaced — and the previous one dropped — on the next open.
thread_local! {
    static CURRENT: RefCell<Option<Retained<HealthController>>> = const { RefCell::new(None) };
}

struct HealthIvars {
    window: Retained<NSWindow>,
    status: Retained<NSTextField>,
    freshness: Retained<NSTextField>,
    autostart: Retained<NSTextField>,
    /// Hand-off slot for the async health check (same pattern as the menu
    /// controller): the background thread stores the [`Report`] here and wakes
    /// `applyReport:`, which paints it on the main thread.
    pending: Arc<Mutex<Option<Report>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FskitS3HealthController"]
    #[ivars = HealthIvars]
    struct HealthController;

    impl HealthController {
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            appkit::open_extensions_settings();
        }

        #[unsafe(method(recheck:))]
        fn recheck(&self, _sender: Option<&AnyObject>) {
            self.refresh();
        }

        // Main-thread continuation of `refresh`: drain the slot the background health
        // check filled and paint the labels. Invoked via `performSelectorOnMainThread:`.
        #[unsafe(method(applyReport:))]
        fn apply_report(&self, _sender: Option<&AnyObject>) {
            let taken = self
                .ivars()
                .pending
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if let Some(report) = taken {
                let iv = self.ivars();
                let (symbol, tint, text) = health_line(&report.health);
                appkit::set_symbol_line(&iv.status, symbol, tint, &text);
                match freshness_line(&report.freshness) {
                    Some((symbol, tint, text)) => {
                        appkit::set_symbol_line(&iv.freshness, symbol, tint, &text)
                    }
                    None => appkit::set_string(&iv.freshness, ""),
                }
                appkit::set_string(&iv.autostart, &autostart_line());
            }
        }
    }

    unsafe impl NSObjectProtocol for HealthController {}
);

impl HealthController {
    fn new(mtm: MainThreadMarker, ivars: HealthIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: `this` is a fresh alloc()+set_ivars, not yet initialized — the precondition of NSObject's designated `-init`.
        unsafe { msg_send![super(this), init] }
    }

    /// Re-run the health check off the main thread and repaint the labels when it
    /// completes (`applyReport:`), so the window never blocks on the FSKit query.
    fn refresh(&self) {
        appkit::set_string(&self.ivars().status, "Checking…");
        let pending = self.ivars().pending.clone();
        let target: &AnyObject = self;
        appkit::run_off_main_then_notify(target, sel!(applyReport:), move || {
            let report = health::check();
            if let Ok(mut slot) = pending.lock() {
                *slot = Some(report);
            }
        });
    }
}

/// The `(SF Symbol, tint, text)` for a health state — the leading status glyph
/// plus its message, painted as one attributed line.
fn health_line(health: &Health) -> (&'static str, Tint, String) {
    match health {
        Health::Ready => (
            "checkmark.circle.fill",
            Tint::Green,
            "Extension enabled and ready.".to_string(),
        ),
        Health::Disabled => (
            "exclamationmark.triangle.fill",
            Tint::Orange,
            "Extension installed but not enabled. Open System Settings to enable it.".to_string(),
        ),
        Health::NotInstalled => (
            "exclamationmark.triangle.fill",
            Tint::Orange,
            "Extension not registered yet — give it a moment, or relaunch the app.".to_string(),
        ),
        Health::Error(msg) => (
            "xmark.octagon.fill",
            Tint::Red,
            format!("Couldn't read extension status: {msg}"),
        ),
    }
}

/// The `(SF Symbol, tint, text)` for the build-freshness state, or `None` when it
/// isn't applicable (the line is then cleared).
fn freshness_line(freshness: &Freshness) -> Option<(&'static str, Tint, String)> {
    Some(match freshness {
        Freshness::Unknown => return None,
        Freshness::Match { sha, dirty: false } => (
            "checkmark.seal.fill",
            Tint::Green,
            format!("Registered build matches this app ({sha})."),
        ),
        Freshness::Match { sha, dirty: true } => (
            "checkmark.seal",
            Tint::Yellow,
            format!(
                "Registered build matches this app ({sha}) — dirty build, so equal SHAs aren't a guarantee."
            ),
        ),
        Freshness::Mismatch { registered, host } => (
            "exclamationmark.triangle.fill",
            Tint::Orange,
            format!(
                "Build mismatch — fskitd will launch a DIFFERENT build.\n\
                 Registered: {registered}\nThis app: {host}\n\
                 Relaunch to re-register; if it persists: sudo killall fskitd"
            ),
        ),
    })
}

/// A line describing the launch-at-login registration state.
fn autostart_line() -> String {
    match autostart::current_status() {
        autostart::Status::Enabled => "Launch at login: on.".to_string(),
        autostart::Status::RequiresApproval => {
            "Launch at login: awaiting your approval in System Settings ▸ Login Items.".to_string()
        }
        autostart::Status::NotRegistered => "Launch at login: off.".to_string(),
        autostart::Status::NotFound | autostart::Status::Unknown => {
            "Launch at login: unavailable (unsigned/dev build).".to_string()
        }
    }
}

/// Open (or re-raise) the health window and refresh it.
pub fn open(mtm: MainThreadMarker) {
    // If it's already open, just refresh + front it rather than stacking windows.
    if let Some(existing) = CURRENT.with(|c| c.borrow().clone()) {
        existing.refresh();
        appkit::show_window(&existing.ivars().window, mtm);
        return;
    }

    const W: f64 = 460.0;
    const H: f64 = 300.0;
    let window = appkit::make_window(mtm, W, H, "fskit-s3");
    let Some(content) = appkit::content_view(&window) else {
        return;
    };

    appkit::add_subview(
        &content,
        &appkit::label(
            mtm,
            appkit::rect(20.0, H - 44.0, W - 40.0, 28.0),
            "fskit-s3 extension",
        ),
    );
    let status =
        appkit::wrapping_label(mtm, appkit::rect(20.0, 176.0, W - 40.0, 64.0), "Checking…");
    appkit::add_subview(&content, &status);
    let freshness = appkit::wrapping_label(mtm, appkit::rect(20.0, 96.0, W - 40.0, 72.0), "");
    appkit::add_subview(&content, &freshness);
    let autostart = appkit::wrapping_label(mtm, appkit::rect(20.0, 60.0, W - 40.0, 28.0), "");
    appkit::add_subview(&content, &autostart);

    let settings = appkit::push_button(
        mtm,
        appkit::rect(20.0, 16.0, 240.0, 32.0),
        "Open System Settings…",
    );
    appkit::set_default_button(&settings);
    appkit::add_subview(&content, &settings);
    let recheck = appkit::push_button(
        mtm,
        appkit::rect(W - 20.0 - 120.0, 16.0, 120.0, 32.0),
        "Re-check",
    );
    appkit::add_subview(&content, &recheck);

    let controller = HealthController::new(
        mtm,
        HealthIvars {
            window: window.clone(),
            status,
            freshness,
            autostart,
            pending: Arc::new(Mutex::new(None)),
        },
    );
    let target: &AnyObject = &controller;
    appkit::set_target_action(&settings, sel!(openSettings:), target);
    appkit::set_target_action(&recheck, sel!(recheck:), target);

    controller.refresh();
    CURRENT.with(|c| *c.borrow_mut() = Some(controller));
    appkit::show_window(&window, mtm);
}

/// The SF Symbols + text for the menu bar and the menu's health row, derived from
/// a report: the menu-bar glyph (`bar_symbol`, template/monochrome) and the row's
/// tinted status symbol + label.
pub struct Glyphs {
    pub bar_symbol: &'static str,
    pub row_symbol: &'static str,
    pub row_tint: Tint,
    pub row_text: String,
}

/// The menu-bar glyph when the extension is healthy, and when it needs attention.
const BAR_OK: &str = "cloud";
const BAR_ALERT: &str = "cloud.bolt";

pub fn menu_glyphs(report: &Report) -> Glyphs {
    // A build mismatch is worth surfacing even when the extension is "ready".
    if matches!(report.freshness, Freshness::Mismatch { .. }) {
        return Glyphs {
            bar_symbol: BAR_ALERT,
            row_symbol: "exclamationmark.triangle.fill",
            row_tint: Tint::Orange,
            row_text: "Extension ready — but a different build will run".to_string(),
        };
    }
    match &report.health {
        Health::Ready => Glyphs {
            bar_symbol: BAR_OK,
            row_symbol: "checkmark.circle.fill",
            row_tint: Tint::Green,
            row_text: "Extension ready".to_string(),
        },
        Health::Disabled => Glyphs {
            bar_symbol: BAR_ALERT,
            row_symbol: "exclamationmark.triangle.fill",
            row_tint: Tint::Orange,
            row_text: "Extension not enabled — click to fix".to_string(),
        },
        Health::NotInstalled => Glyphs {
            bar_symbol: BAR_ALERT,
            row_symbol: "exclamationmark.triangle.fill",
            row_tint: Tint::Orange,
            row_text: "Extension not registered — click to fix".to_string(),
        },
        Health::Error(_) => Glyphs {
            bar_symbol: BAR_ALERT,
            row_symbol: "xmark.octagon.fill",
            row_tint: Tint::Red,
            row_text: "Extension status unavailable".to_string(),
        },
    }
}
