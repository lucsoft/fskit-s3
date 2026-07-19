//! The Rust↔Swift **contract** — the whole surface the SwiftUI app calls.
//!
//! Everything above this line is native SwiftUI (menu bar, forms, windows);
//! everything the UI *does* — health checks, the connection registry, Keychain
//! secrets, mounting, the S3 credential test — is a `#[uniffi::export]` function
//! here, over the same pure Rust modules the app has always used. UniFFI compiles
//! these into a typed Swift API (records/enums/`throws`), so the boundary is
//! checked at build time rather than hand-marshalled across a C ABI.
//!
//! Presentation stays on the Swift side on purpose: this module returns *state*
//! (a [`Report`], a [`Connection`], a mount list), and SwiftUI decides how to draw
//! it (which SF Symbol, which colour, which window). The one thing that must not
//! leak across the boundary — the S3 secret — is handled here (Keychain or a
//! single `-o secret` mount) and **never crosses back to Swift**: the edit form only
//! learns *whether* a secret exists ([`has_secret`]) and, when the user leaves that
//! placeholder untouched, [`save_connection`] reuses the stored one.

use crate::connection::{self, Connection, ConnectionKind, FormInput, Registry, S3Meta};
use crate::health::Report;
use crate::mounts::{self, Mount};
use crate::{disksecret, keychain, s3check};

/// A connection's S3 secret for a mount, resolved by [`secret_plan`].
///
/// The app **always hands the secret to the extension via `-o secret`** when it has
/// one. We deliberately do *not* try to mount with `secret = None` in the hope that
/// the extension reads the shared Keychain group itself: the app being able to read a
/// shared-group item does **not** mean the *extension* can. On an unsigned build the
/// app's shared-group read succeeds while the extension's fails, so that "secure" path
/// mounts with nothing and the extension dies with `no secret` — the exact failure
/// this avoids. (On a properly signed build the extension still prefers its own
/// Keychain read and ignores the `-o` value, so nothing is lost there.)
enum SecretPlan {
    /// A secret the app could read (dev disk file first, then any Keychain copy) —
    /// passed to the mount as `-o secret`.
    Supply(String),
    /// No secret anywhere — the caller must prompt (or skip, at launch).
    Missing,
}

/// Reject a user-chosen mount folder that isn't an empty directory — mounting over a
/// non-empty one would hide its contents. `.`-prefixed entries (e.g. `.DS_Store`) are
/// ignored, so a folder Finder merely visited still counts as empty. A folder that
/// doesn't exist yet is fine (created at mount time), and no custom folder (the
/// default `~/fskit-s3/<name>`) is exempt.
fn validate_mount_point(conn: &Connection) -> Result<(), FfiError> {
    let Some(custom) = conn.mount_point.as_deref().filter(|p| !p.is_empty()) else {
        return Ok(());
    };
    let path = std::path::Path::new(custom);
    if !path.exists() {
        return Ok(());
    }
    if !path.is_dir() {
        return Err(FfiError::from(format!(
            "Mount folder {custom:?} is not a folder."
        )));
    }
    let entries = std::fs::read_dir(path)
        .map_err(|e| FfiError::from(format!("Can't read mount folder {custom:?}: {e}")))?;
    let has_visible = entries.filter_map(Result::ok).any(|e| {
        e.file_name()
            .to_str()
            .map(|n| !n.starts_with('.'))
            .unwrap_or(true)
    });
    if has_visible {
        return Err(FfiError::from(format!(
            "Mount folder {custom:?} isn't empty — pick an empty folder (mounting over \
             it would hide its contents)."
        )));
    }
    Ok(())
}

/// The secret to hand the mount: the dev on-disk **raw password** first (the user's
/// explicit choice), then any Keychain copy; [`SecretPlan::Missing`] when there's none.
fn secret_plan(name: &str) -> SecretPlan {
    match disksecret::read(name).or_else(|| keychain::read_secret(name)) {
        Some(secret) => SecretPlan::Supply(secret),
        None => SecretPlan::Missing,
    }
}

/// The error every fallible contract call surfaces to Swift. UniFFI turns it into
/// a Swift `Error`; the SwiftUI layer switches on the variant.
#[derive(Debug, uniffi::Error)]
pub enum FfiError {
    /// A human-readable failure reason (mount stderr, a validation message, a
    /// Keychain error). Shown to the user as-is.
    Message { message: String },
    /// An S3 mount was attempted with no usable secret (none stored, or an unsigned
    /// build can't read the shared Keychain). The UI responds by prompting for it.
    NeedsSecret,
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::Message { message } => f.write_str(message),
            FfiError::NeedsSecret => f.write_str("this connection needs its S3 secret"),
        }
    }
}

impl std::error::Error for FfiError {}

impl From<String> for FfiError {
    fn from(message: String) -> Self {
        FfiError::Message { message }
    }
}

// --- Extension health ------------------------------------------------------

/// Query FSKit for the extension's install/enable state and build freshness.
///
/// **Blocks** briefly on a local XPC round-trip (see [`crate::health::check`]), so
/// Swift must call it off the main actor (a `Task.detached`) and apply the result
/// back on the main actor — exactly what the old Rust UI did by hand.
#[uniffi::export]
pub fn check_health() -> Report {
    crate::health::check()
}

// --- Launch at login -------------------------------------------------------

/// The current launch-at-login registration status.
#[uniffi::export]
pub fn autostart_status() -> crate::autostart::Status {
    crate::autostart::current_status()
}

/// Register the app to launch at login (best-effort; a no-op if already enabled).
#[uniffi::export]
pub fn enable_autostart() {
    crate::autostart::enable();
}

// --- Connections -----------------------------------------------------------

/// All configured connections, in registry order. Loaded fresh from disk each
/// call, so the menu always reflects the latest saves/edits/deletes.
#[uniffi::export]
pub fn list_connections() -> Vec<Connection> {
    Registry::load().list().to_vec()
}

/// The mount point for a connection — its custom folder if set, else the default
/// `~/fskit-s3/<name>`. The menu joins this against [`list_fskit_mounts`] to show a
/// green/grey "mounted" dot, and unmount targets it, so it must match what
/// [`mount_connection`] actually mounts. Falls back to the default for an unknown
/// name.
#[uniffi::export]
pub fn mount_point_for(name: String) -> String {
    Registry::load()
        .get(&name)
        .map(|c| c.mount_point())
        .unwrap_or_else(|| connection::default_mount_point_for(&name))
        .to_string_lossy()
        .into_owned()
}

/// Whether a connection currently has a stored secret — the Keychain (shared group,
/// then default) or the dev-only on-disk file. Used to pre-decide whether mounting
/// will need a prompt.
#[uniffi::export]
pub fn has_secret(name: String) -> bool {
    keychain::read_secret(&name).is_some() || disksecret::read(&name).is_some()
}

/// Validate raw form values into a [`Connection`] **without** any network or
/// disk I/O — for inline form feedback. Returns the connection or a field error.
#[uniffi::export]
pub fn validate_form(form: FormInput) -> Result<Connection, FfiError> {
    Connection::from_form(form).map_err(FfiError::from)
}

/// Validate + **save** a connection (the form's "Test & Save"): for S3, list the
/// bucket to confirm the credentials, store the secret in the Keychain when asked,
/// then persist the connection. `original_name` is `Some` when editing (the prior
/// entry, whose name is locked, is replaced in place) and `None` when creating (the
/// name must be free). Returns the saved connection.
#[uniffi::export]
pub fn save_connection(
    form: FormInput,
    original_name: Option<String>,
) -> Result<Connection, FfiError> {
    let save_keychain = form.save_secret_to_keychain;
    let save_disk = form.save_secret_to_disk;

    // On edit, "keep current password" (the form left its stored-secret placeholder
    // untouched) reuses the stored secret (Keychain or disk), so changing other fields
    // doesn't force a re-type. This is distinct from a *blank* Secret field, which
    // means an empty secret (and for S3 then fails the required-secret check).
    let mut form = form;
    if form.is_s3 && form.keep_stored_secret {
        if let Some(existing) = original_name
            .as_deref()
            .and_then(|orig| keychain::read_secret(orig).or_else(|| disksecret::read(orig)))
        {
            form.secret = existing;
        }
    }
    // Keep what the live check + secret storage need before `from_form` consumes `form`.
    let secret = form.secret.clone();

    let conn = Connection::from_form(form).map_err(FfiError::from)?;

    // A newly chosen custom mount folder must be an empty directory (checked before the
    // network test so it fails fast). On edit, only when the folder actually changed —
    // an unchanged one may be legitimately non-empty (e.g. currently mounted). The
    // default `~/fskit-s3/<name>` (mount_point == None) is exempt inside the validator.
    let previous_mount_point = original_name.as_deref().and_then(|orig| {
        Registry::load()
            .get(orig)
            .and_then(|c| c.mount_point.clone())
    });
    if conn.mount_point != previous_mount_point {
        validate_mount_point(&conn)?;
    }

    if let ConnectionKind::S3(meta) = &conn.kind {
        s3check::test_s3(meta, &secret)
            .map_err(|e| FfiError::from(format!("Couldn't reach the bucket: {e}")))?;
        if save_keychain {
            keychain::store_secret(&conn.name, &secret)
                .map_err(|e| FfiError::from(format!("Keychain save failed: {e}")))?;
        }
        // Dev-only plaintext fallback (unsigned builds the ext can't read the
        // Keychain from). Written when asked; cleared otherwise so an un-ticked box
        // removes a stale file left by a previous save.
        if save_disk {
            disksecret::store(&conn.name, &secret)
                .map_err(|e| FfiError::from(format!("Disk secret save failed: {e}")))?;
        } else {
            disksecret::delete(&conn.name);
        }
    }

    let mut registry = Registry::load();
    match &original_name {
        // Editing: drop the previous (locked-name) entry and re-add in its place.
        Some(orig) => {
            registry.remove(orig);
        }
        // Creating: the name must be free.
        None => {
            if registry.get(&conn.name).is_some() {
                return Err(FfiError::from(format!(
                    "A connection named {:?} already exists.",
                    conn.name
                )));
            }
        }
    }
    registry.add(conn.clone()).map_err(FfiError::from)?;
    registry
        .save()
        .map_err(|e| FfiError::from(format!("Save failed: {e}")))?;
    Ok(conn)
}

/// Delete a connection: unmount it first if mounted (aborting on failure, so a live
/// mount is never orphaned with its config gone), then drop it from the registry
/// and its secret from the Keychain. The UI runs its own confirmation first.
#[uniffi::export]
pub fn delete_connection(name: String) -> Result<(), FfiError> {
    let mut registry = Registry::load();
    if let Some(conn) = registry.get(&name) {
        let mount_point = conn.mount_point();
        let mount_point = mount_point.to_string_lossy();
        let mounted = mounts::list_fskit()
            .iter()
            .any(|m| m.mount_point == *mount_point);
        if mounted {
            mounts::unmount(&mount_point)
                .map_err(|e| FfiError::from(format!("Couldn't unmount: {e}")))?;
        }
    }
    registry.remove(&name);
    registry
        .save()
        .map_err(|e| FfiError::from(format!("Delete failed: {e}")))?;
    keychain::delete_secret(&name);
    disksecret::delete(&name);
    Ok(())
}

/// Validate S3 credentials by listing the bucket root (the form's standalone
/// "Test" action, without saving).
#[uniffi::export]
pub fn test_s3(meta: S3Meta, secret: String) -> Result<(), FfiError> {
    s3check::test_s3(&meta, &secret).map_err(FfiError::from)
}

// --- Mounting --------------------------------------------------------------

/// Mounts served by this filesystem (the `mount` rows whose type contains `fskit`).
#[uniffi::export]
pub fn list_fskit_mounts() -> Vec<Mount> {
    mounts::list_fskit()
}

/// Mount a saved connection using its stored secret. A Keychain-stored secret is
/// read by the extension itself (no `-o secret` on the command line); the dev-only
/// on-disk plaintext secret is read here and passed via `-o secret`. An S3 connection
/// with no usable secret raises [`FfiError::NeedsSecret`], and a mount the extension
/// rejects for a missing/unreadable secret is mapped to the same — so the UI can
/// prompt. Other failures come back as [`FfiError::Message`] with an actionable hint.
#[uniffi::export]
pub fn mount_connection(name: String) -> Result<(), FfiError> {
    let registry = Registry::load();
    let Some(conn) = registry.get(&name) else {
        return Err(FfiError::from(format!("No connection named {name:?}.")));
    };
    let secret = if conn.is_s3() {
        match secret_plan(&name) {
            SecretPlan::Missing => return Err(FfiError::NeedsSecret),
            SecretPlan::Supply(s) => Some(s),
        }
    } else {
        None
    };
    match mounts::mount(conn, &conn.mount_point(), secret.as_deref()) {
        Ok(()) => Ok(()),
        Err(e) => Err(mount_error(&e, conn.is_s3())),
    }
}

/// Mount an S3 connection with a secret supplied now (the prompt path): persist it
/// first when asked — to the Keychain (secure) and/or the dev-only on-disk plaintext
/// file — then mount with `-o secret` (the insecure path, for when the ext can't read
/// the shared Keychain on an unsigned build). Persisting to disk is what lets a later
/// one-click or launch mount reuse the secret on an unsigned build without re-typing.
#[uniffi::export]
pub fn mount_with_secret(
    name: String,
    secret: String,
    save_to_keychain: bool,
    save_to_disk: bool,
) -> Result<(), FfiError> {
    let registry = Registry::load();
    let Some(conn) = registry.get(&name) else {
        return Err(FfiError::from(format!("No connection named {name:?}.")));
    };
    if save_to_keychain {
        // Best-effort — the mount can still proceed via `-o secret` if this fails.
        let _ = keychain::store_secret(&name, &secret);
    }
    if save_to_disk {
        // Best-effort likewise; the mount below carries the secret regardless.
        let _ = disksecret::store(&name, &secret);
    }
    mounts::mount(conn, &conn.mount_point(), Some(&secret))
        .map_err(|e| mount_error(&e, conn.is_s3()))
}

/// Unmount a volume by its mount point (`diskutil unmount`, the clean path that also
/// clears fskitd's mount-point record).
#[uniffi::export]
pub fn unmount(mount_point: String) -> Result<(), FfiError> {
    mounts::unmount(&mount_point).map_err(FfiError::from)
}

/// A connection that failed to auto-mount at launch, with the reason — so the log
/// (and any future UI) shows *why*, not just which one.
#[derive(Debug, Clone, uniffi::Record)]
pub struct AutoMountFailure {
    pub connection: String,
    pub reason: String,
}

/// Mount every connection flagged `mount_on_launch` whose secret is available
/// (S3 connections without a stored secret are skipped — a prompt can't run
/// unattended at launch). Best-effort; returns the connections that failed **with the
/// mount error**, for logging.
#[uniffi::export]
pub fn auto_mount_on_launch() -> Vec<AutoMountFailure> {
    let mut failed = Vec::new();
    for conn in Registry::load().list() {
        if !conn.mount_on_launch {
            continue;
        }
        let secret = if conn.is_s3() {
            match secret_plan(&conn.name) {
                // No secret to mount with unattended (a prompt can't run at launch).
                SecretPlan::Missing => continue,
                SecretPlan::Supply(s) => Some(s),
            }
        } else {
            None
        };
        if let Err(reason) = mounts::mount(conn, &conn.mount_point(), secret.as_deref()) {
            failed.push(AutoMountFailure {
                connection: conn.name.clone(),
                reason,
            });
        }
    }
    failed
}

/// The launch-flagged S3 connections whose secret **isn't** available for an
/// unattended mount — the ones [`auto_mount_on_launch`] had to skip. The UI opens a
/// secret prompt for each so "mount when launching" still mounts (with the user
/// typing the password) instead of silently doing nothing. Memory connections and
/// those with a usable secret are omitted (they auto-mount headlessly).
#[uniffi::export]
pub fn pending_secret_mounts_on_launch() -> Vec<String> {
    Registry::load()
        .list()
        .iter()
        .filter(|c| c.mount_on_launch && c.is_s3())
        .filter(|c| matches!(secret_plan(&c.name), SecretPlan::Missing))
        .map(|c| c.name.clone())
        .collect()
}

/// Cleanly unmount every volume this app serves — called on quit. A clean (non-
/// force) unmount removes fskitd's mount-point record, avoiding the `Code=516`
/// "already exists" orphan a later crash/kill would leave. Non-force, so a busy
/// volume stays mounted rather than being yanked.
#[uniffi::export]
pub fn unmount_all_on_quit() {
    for m in mounts::list_fskit() {
        if m.fs_type != mounts::FS_TYPE {
            continue;
        }
        let _ = mounts::unmount(&m.mount_point);
    }
}

/// Map a `mount` failure into a typed [`FfiError`]: a missing/unreadable S3 secret
/// (EINVAL on an S3 mount) becomes [`FfiError::NeedsSecret`] so the UI prompts;
/// everything else becomes a [`FfiError::Message`] with a specific, actionable hint
/// (stale fskitd record → `sudo killall fskitd`; busy → unmount first).
fn mount_error(err: &str, is_s3: bool) -> FfiError {
    if err.contains("already exists") || err.contains("Code=516") {
        FfiError::from(format!(
            "A leftover FSKit mount record is blocking this mount point — a previous \
             mount didn't unmount cleanly. Clearing it needs a daemon restart (fskitd \
             runs as root). In Terminal, run:\n\n    sudo killall fskitd\n\nthen try \
             mounting again.\n\nDetails: {err}"
        ))
    } else if err.contains("Resource busy") {
        FfiError::from(format!(
            "Something is already mounted at this location. Unmount it first, then try \
             again.\n\nDetails: {err}"
        ))
    } else if is_s3 && (err.contains("Invalid argument") || err.contains("Code=22")) {
        FfiError::NeedsSecret
    } else {
        FfiError::from(format!("Details: {err}"))
    }
}
