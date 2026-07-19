//! FSKit extension health, via `FSClient` (the same query the old standalone host
//! window ran, ported into the app so the menu-bar item can show it).
//!
//! [`check`] asks FSKit whether our module is installed + enabled and, if so,
//! whether the build FSKit will actually launch matches this app — the
//! content-based staleness signal (git SHA in each bundle's Info.plist; mtimes
//! lie, git rewrites them on checkout).
//!
//! `FSClient.fetchInstalledExtensionsWithCompletionHandler:` is asynchronous
//! (XPC to `fskit_agent`). We bridge it to a blocking call with a channel: the
//! completion block — which captures only the `Sender` (nothing UI, nothing
//! thread-bound) — computes the [`Report`] and sends it; the caller waits with a
//! short timeout. On timeout we report an error rather than hang.
//!
//! [`check`] **blocks**, so it's exported through the contract and the SwiftUI side
//! calls it **off the main actor** (a `Task.detached`), applying the result back on
//! the main actor — the UI never waits on the XPC round-trip.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;
use std::sync::mpsc;
use std::time::Duration;

/// Our extension's bundle identifier (must match `Extension-Info.plist`).
const EXTENSION_BUNDLE_ID: &str = "dev.lucsoft.fskit-s3.ext";
/// The Info.plist key both bundles carry their git SHA under (see stamp-git-sha.sh).
const SHA_KEY: &str = "FSKitS3GitSHA";
/// How long to wait for `fskit_agent` before giving up (it's local XPC — fast).
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether our module is installed with FSKit and enabled by the user.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum Health {
    /// FSKit has no record of our module yet (not registered, or still settling).
    NotInstalled,
    /// Installed, but the user hasn't enabled it in System Settings.
    Disabled,
    /// Installed and enabled — mounts can run.
    Ready,
    /// The query itself failed (XPC error, timeout).
    Error(String),
}

/// How the build FSKit will launch compares to the one this app ships.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum Freshness {
    /// Not determinable (a pre-SHA build, or health couldn't be read).
    Unknown,
    /// Same git SHA as this app. `dirty` ⇒ built from an uncommitted tree, so
    /// equal SHAs aren't conclusive (two dirty builds can share one).
    Match { sha: String, dirty: bool },
    /// A different build will run than the one this app embeds.
    Mismatch { registered: String, host: String },
}

/// The result of a health check: install/enable state + build freshness.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Report {
    pub health: Health,
    pub freshness: Freshness,
}

impl Report {
    fn error(msg: impl Into<String>) -> Self {
        Report {
            health: Health::Error(msg.into()),
            freshness: Freshness::Unknown,
        }
    }
}

/// Query FSKit for our module's health. Blocks the caller (briefly) until FSKit
/// replies or [`CHECK_TIMEOUT`] elapses. Safe to call on the main thread — the
/// completion runs on a background queue, so the wait can't deadlock.
pub fn check() -> Report {
    let client = match fsclient_shared() {
        Some(c) => c,
        None => return Report::error("FSKit unavailable"),
    };

    let (tx, rx) = mpsc::channel::<Report>();
    // The completion block captures only `tx` (Send + 'static): no UI, no
    // thread-bound state. FSKit copies the block and calls it once on its own
    // queue; a late call after we've stopped listening just fails `send` (ignored).
    let handler = RcBlock::new(move |modules: *mut AnyObject, error: *mut AnyObject| {
        let _ = tx.send(build_report(modules, error));
    });

    // SAFETY: `client` is the live shared FSClient; the selector takes a
    // `void(^)(NSArray*, NSError*)` completion, which `handler` is.
    unsafe {
        let _: () = msg_send![&client, fetchInstalledExtensionsWithCompletionHandler: &*handler];
    }

    match rx.recv_timeout(CHECK_TIMEOUT) {
        Ok(report) => report,
        Err(_) => Report::error("timed out asking FSKit (fskit_agent)"),
    }
}

/// The shared `FSClient` instance (`+[FSClient sharedInstance]`).
fn fsclient_shared() -> Option<Retained<AnyObject>> {
    // SAFETY: `FSClient` is an FSKit class (linked via build.rs); `sharedInstance`
    // is a readonly class property returning the shared singleton.
    unsafe {
        let cls = class!(FSClient);
        let client: *mut AnyObject = msg_send![cls, sharedInstance];
        Retained::retain(client)
    }
}

/// Turn the completion's `(NSArray<FSModuleIdentity>*, NSError*)` into a [`Report`].
/// Runs on FSKit's queue; touches only ObjC reads + the returned value.
fn build_report(modules: *mut AnyObject, error: *mut AnyObject) -> Report {
    // SAFETY: both args are the completion's parameters — a nullable NSArray and a
    // nullable NSError. We only read them (count/objectAtIndex, localizedDescription).
    unsafe {
        if let Some(err) = error.as_ref() {
            let desc: *mut NSString = msg_send![err, localizedDescription];
            let msg = desc
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown FSKit error".to_string());
            return Report::error(msg);
        }
        let Some(modules) = modules.as_ref() else {
            return Report::error("FSKit returned no module list");
        };

        let count: usize = msg_send![modules, count];
        for i in 0..count {
            let module: *mut AnyObject = msg_send![modules, objectAtIndex: i];
            let Some(module) = module.as_ref() else {
                continue;
            };
            let bid: *mut NSString = msg_send![module, bundleIdentifier];
            let matches = bid
                .as_ref()
                .map(|s| s.to_string() == EXTENSION_BUNDLE_ID)
                .unwrap_or(false);
            if !matches {
                continue;
            }
            let enabled: bool = msg_send![module, isEnabled];
            let url: *mut AnyObject = msg_send![module, url];
            let freshness = freshness_for(url);
            return Report {
                health: if enabled {
                    Health::Ready
                } else {
                    Health::Disabled
                },
                freshness,
            };
        }
        Report {
            health: Health::NotInstalled,
            freshness: Freshness::Unknown,
        }
    }
}

/// Compare the git SHA of the extension bundle at `registered_url` (the one FSKit
/// will launch) against this app's own SHA. Stays [`Freshness::Unknown`] unless
/// both bundles carry a real SHA, so a pre-SHA build doesn't raise a false alarm.
///
/// # Safety
/// `registered_url` must be a valid (possibly null) `NSURL*` from FSKit.
unsafe fn freshness_for(registered_url: *mut AnyObject) -> Freshness {
    let Some(registered) = sha_at_url(registered_url) else {
        return Freshness::Unknown;
    };
    let Some(host) = host_sha() else {
        return Freshness::Unknown;
    };
    if !is_real_sha(&registered) || !is_real_sha(&host) {
        return Freshness::Unknown;
    }
    if registered == host {
        Freshness::Match {
            dirty: registered.ends_with("-dirty"),
            sha: registered,
        }
    } else {
        Freshness::Mismatch { registered, host }
    }
}

/// A SHA is a usable identity when present and not the `unknown` placeholder.
fn is_real_sha(sha: &str) -> bool {
    !sha.is_empty() && sha != "unknown"
}

/// This app's own git SHA, from its `Info.plist` (`+[NSBundle mainBundle]`).
fn host_sha() -> Option<String> {
    // SAFETY: `mainBundle` is a class method returning the (non-null) main bundle.
    unsafe {
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        sha_in_bundle(bundle)
    }
}

/// The git SHA of the bundle at `url` (`+[NSBundle bundleWithURL:]`).
///
/// # Safety
/// `url` must be a valid (possibly null) `NSURL*`.
unsafe fn sha_at_url(url: *mut AnyObject) -> Option<String> {
    if url.is_null() {
        return None;
    }
    let bundle: *mut AnyObject = msg_send![class!(NSBundle), bundleWithURL: url];
    sha_in_bundle(bundle)
}

/// Read the `FSKitS3GitSHA` Info.plist value from a bundle pointer.
///
/// # Safety
/// `bundle` must be a valid (possibly null) `NSBundle*`.
unsafe fn sha_in_bundle(bundle: *mut AnyObject) -> Option<String> {
    let bundle = bundle.as_ref()?;
    let key = NSString::from_str(SHA_KEY);
    let value: *mut NSString = msg_send![bundle, objectForInfoDictionaryKey: &*key];
    value.as_ref().map(|s| s.to_string())
}
