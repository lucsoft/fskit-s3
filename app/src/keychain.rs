//! macOS Keychain storage for S3 secret access keys.
//!
//! A secret is a generic password (service [`SERVICE`], account = connection
//! name). When possible it's placed in a **shared keychain access group** so the
//! FSKit extension can read it too; that needs a signed app holding the matching
//! `keychain-access-groups` entitlement, so on an unsigned dev build we
//! transparently fall back to the app's default keychain (still readable by the
//! app, just not shared with the extension).
//!
//! Built on the safe `security-framework` API — this module writes no `unsafe`.

use security_framework::passwords::{
    delete_generic_password_options, generic_password, set_generic_password_options,
    PasswordOptions,
};

/// Keychain service name shared by all fskit-s3 secrets.
const SERVICE: &str = "dev.lucsoft.fskit-s3";

/// The shared access group (team-id prefixed) the extension also holds via its
/// `keychain-access-groups` entitlement. Only effective for a signed app; ignored
/// (falls back to the default keychain) otherwise.
const ACCESS_GROUP: &str = "H8563U643B.dev.lucsoft.fskit-s3";

fn options(account: &str, shared: bool) -> PasswordOptions {
    let mut o = PasswordOptions::new_generic_password(SERVICE, account);
    if shared {
        o.set_access_group(ACCESS_GROUP);
    }
    o
}

/// Store (or update) the secret for a connection. Prefers the shared access group
/// so the extension can read it, falling back to the default keychain when the app
/// lacks the entitlement (unsigned dev).
pub fn store_secret(name: &str, secret: &str) -> Result<(), String> {
    let bytes = secret.as_bytes();
    match set_generic_password_options(bytes, options(name, true)) {
        Ok(()) => Ok(()),
        Err(_) => {
            set_generic_password_options(bytes, options(name, false)).map_err(|e| e.to_string())
        }
    }
}

/// Read a connection's secret from the **shared access group only** — the item the
/// *extension* can read. A hit here means a mount can rely on `Keychain[name]`
/// (no `-o secret`); a miss means the secret is at best in the app-only default
/// keychain, which the sandboxed extension can't see. On an unsigned build (no
/// entitlement) the shared-group query never matches, so this returns `None` — which
/// is exactly the signal the mount path needs to fall back to passing `-o secret`.
pub fn read_shared_secret(name: &str) -> Option<String> {
    let bytes = generic_password(options(name, true)).ok()?;
    String::from_utf8(bytes).ok()
}

/// Read a connection's secret from the **default keychain only** — readable by the
/// app but NOT by the sandboxed extension, so a mount must hand it over via
/// `-o secret`. This is where the secret lands on an unsigned build (the shared-group
/// store falls back here).
pub fn read_default_secret(name: &str) -> Option<String> {
    let bytes = generic_password(options(name, false)).ok()?;
    String::from_utf8(bytes).ok()
}

/// Read a connection's secret from anywhere the app can (shared group first, then
/// default) — for pre-filling the edit form, where only *whether* a secret exists and
/// its value matter, not which store the extension can reach.
pub fn read_secret(name: &str) -> Option<String> {
    read_shared_secret(name).or_else(|| read_default_secret(name))
}

/// Delete a connection's secret from both the shared group and the default keychain.
pub fn delete_secret(name: &str) {
    let _ = delete_generic_password_options(options(name, true));
    let _ = delete_generic_password_options(options(name, false));
}
