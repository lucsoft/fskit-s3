//! Dev-mode plaintext secret storage on local disk — an **insecure** alternative
//! to the Keychain ([`crate::keychain`]) for unsigned dev builds.
//!
//! On a signed build an S3 secret lives in a shared Keychain access group the FSKit
//! extension reads directly, so a mount needn't put the secret on the command line.
//! An unsigned dev build has no usable shared group (the extension can't read the
//! app's Keychain), so every mount would re-prompt. This module is the escape hatch:
//! the secret is written as a `0600` plaintext file the app owns, and at mount time
//! the app reads it back and hands it to the extension via `-o secret`
//! ([`crate::mounts::mount`]) — the same insecure path the prompt uses, but persisted
//! so one-click and launch-time mounts work without re-typing.
//!
//! **This stores the secret in the clear** (and it rides the mount command line,
//! visible in `ps`/`mount`). It's a developer convenience, opt-in per connection via
//! `save_secret_to_disk`, never the default. The file lives *outside*
//! `connections.json` (that file never holds a secret) in a sibling `secrets/` dir.
//!
//! Pure filesystem I/O (no `objc2`), so it unit-tests trivially against a temp dir.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Store (or overwrite) a connection's secret as a `0600` plaintext file.
pub fn store(name: &str, secret: &str) -> Result<(), String> {
    store_in(&app_support_dir(), name, secret)
}

/// Read a connection's on-disk secret, if present.
pub fn read(name: &str) -> Option<String> {
    read_in(&app_support_dir(), name)
}

/// Remove a connection's on-disk secret (best-effort; a missing file is fine).
pub fn delete(name: &str) {
    delete_in(&app_support_dir(), name)
}

/// The per-connection secret file under `base` (`<base>/secrets/<name>`), or `None`
/// for a name that could escape the directory. Names are already validated on the
/// way in (`Connection::from_form` restricts them to `[A-Za-z0-9._-]`); this is a
/// defensive second gate so a stray `/` or `..` can never write outside `secrets/`.
fn secret_path(base: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return None;
    }
    Some(base.join("secrets").join(name))
}

fn store_in(base: &Path, name: &str, secret: &str) -> Result<(), String> {
    let path =
        secret_path(base, name).ok_or_else(|| format!("invalid connection name {name:?}"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        // Best-effort tighten of the containing dir (it only ever holds secrets).
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    // Create/truncate with `0600` so the plaintext secret isn't group/world-readable.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    file.write_all(secret.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    // `mode` only applies when *creating* the file; if it pre-existed with looser
    // permissions, tighten it explicitly.
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    Ok(())
}

fn read_in(base: &Path, name: &str) -> Option<String> {
    let path = secret_path(base, name)?;
    let bytes = fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

fn delete_in(base: &Path, name: &str) {
    if let Some(path) = secret_path(base, name) {
        let _ = fs::remove_file(path);
    }
}

/// The app-local base dir (`~/Library/Application Support/fskit-s3`), matching
/// [`crate::connection`]'s config dir so the `secrets/` folder sits beside
/// `connections.json`.
fn app_support_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => {
            PathBuf::from(home).join("Library/Application Support/fskit-s3")
        }
        _ => PathBuf::from("/tmp/fskit-s3"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fskit-s3-secret-test-{}-{tag}", std::process::id()))
    }

    #[test]
    fn store_read_delete_roundtrip() {
        let base = temp_base("roundtrip");
        let _ = fs::remove_dir_all(&base);

        assert!(read_in(&base, "photos").is_none(), "absent before store");
        store_in(&base, "photos", "s3cr3t").unwrap();
        assert_eq!(read_in(&base, "photos").as_deref(), Some("s3cr3t"));

        // The file must be user-only (0600) so the plaintext isn't broadly readable.
        let path = secret_path(&base, "photos").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secret file must be 0600");

        // Overwrite works and keeps 0600.
        store_in(&base, "photos", "rotated").unwrap();
        assert_eq!(read_in(&base, "photos").as_deref(), Some("rotated"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        delete_in(&base, "photos");
        assert!(read_in(&base, "photos").is_none(), "absent after delete");
        delete_in(&base, "photos"); // deleting a missing secret is fine

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rejects_unsafe_names() {
        let base = temp_base("unsafe");
        for bad in ["", "../evil", "a/b", "..", "sub/../../x"] {
            assert!(
                store_in(&base, bad, "x").is_err(),
                "{bad:?} should be rejected"
            );
            assert!(read_in(&base, bad).is_none(), "{bad:?} should read None");
        }
        // A valid name is accepted right next to the rejected ones.
        assert!(store_in(&base, "ok-name_1.2", "x").is_ok());
        let _ = fs::remove_dir_all(&base);
    }
}
