//! Configured connections — the things you can mount.
//!
//! A [`Connection`] names a storage endpoint; a [`Registry`] holds the set of
//! them and persists it to an app-local JSON file. All pure data + filesystem
//! I/O (no `objc2`), so it unit-tests trivially and stays separate from the app's
//! AppKit/`unsafe` layer.
//!
//! **Secrets never live here.** An [`S3Meta`] carries only non-secret fields; the
//! secret access key is handled by [`crate::keychain`] (secure) or passed to a
//! single mount via `-o` (insecure) — see [`crate::mounts`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A configured storage connection: an identity, the backend it maps to, and how
/// it should be mounted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct Connection {
    pub name: String,
    pub kind: ConnectionKind,
    /// Whether the secret (S3 only) is stored in the Keychain. When false the
    /// secret isn't persisted and a mount must supply it (prompt or `-o secret`).
    #[serde(default)]
    pub save_secret_to_keychain: bool,
    /// Whether the secret (S3 only) is stored as a **plaintext file on disk** — the
    /// insecure dev fallback for unsigned builds where the extension can't read the
    /// shared Keychain group. When set, the app reads it back at mount and passes it
    /// via `-o secret`. Never the default; the secret itself lives in [`crate::disksecret`],
    /// not this struct or `connections.json`.
    #[serde(default)]
    pub save_secret_to_disk: bool,
    /// Mount this connection automatically when the app launches.
    #[serde(default)]
    pub mount_on_launch: bool,
    /// A custom mount point (an empty folder the user picked at creation). `None`
    /// (or empty) ⇒ the default `~/fskit-s3/<name>`. Resolved via
    /// [`Connection::mount_point`].
    #[serde(default)]
    pub mount_point: Option<String>,
}

/// Which backend a connection is served by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum ConnectionKind {
    /// The credential-free in-memory demo tree the extension serves.
    Memory,
    /// An S3 (or S3-compatible) bucket. Non-secret config only.
    S3(S3Meta),
}

/// Non-secret S3 connection config (mirrors `fskit_s3_backend::S3Config` minus the
/// `secret_access_key`, which never touches this struct or the config file).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, uniffi::Record)]
pub struct S3Meta {
    pub bucket: String,
    pub region: String,
    /// Custom endpoint for S3-compatible stores (MinIO, R2, RustFS). Empty ⇒ AWS.
    pub endpoint: String,
    pub access_key_id: String,
    pub session_token: Option<String>,
}

impl Connection {
    /// The built-in, credential-free in-memory connection.
    pub fn memory() -> Self {
        Connection {
            name: "memory".to_string(),
            kind: ConnectionKind::Memory,
            save_secret_to_keychain: false,
            save_secret_to_disk: false,
            mount_on_launch: false,
            mount_point: None,
        }
    }

    /// Whether this is an S3 connection (needs a secret to mount).
    pub fn is_s3(&self) -> bool {
        matches!(self.kind, ConnectionKind::S3(_))
    }

    /// The default mount point for this connection (`~/fskit-s3/<name>`), used when
    /// no custom [`mount_point`](Connection::mount_point) is set.
    pub fn default_mount_point(&self) -> PathBuf {
        default_mount_point_for(&self.name)
    }

    /// Where this connection actually mounts: the user's chosen folder if set,
    /// otherwise [`default_mount_point`](Connection::default_mount_point). This is the
    /// path handed to `mount` (distinct from the config-carrying `source_path`).
    pub fn mount_point(&self) -> PathBuf {
        match &self.mount_point {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => self.default_mount_point(),
        }
    }

    /// The **source path** handed to `mount` — the connection's config, made
    /// self-describing so the extension resolves it at `loadResource` (where a bad
    /// config cleanly unwinds), not from `-o` options at `activate`.
    ///
    /// `/memory` for the demo, or `/s3/<name>?bucket=..&access_key_id=..&region=..&
    /// endpoint=..[&session_token=..]` for S3. `name` (the path segment) is the
    /// extension's Keychain account. The **secret** is never here — it comes from
    /// the Keychain, or [`crate::mounts::mount`] passes an `-o secret` for the
    /// insecure path. The path need not exist on disk. Config values must not
    /// contain the query delimiters `?&=#` (validated in [`Connection::from_form`]);
    /// the extension parses this with `parse_source_path`.
    pub fn source_path(&self) -> String {
        match &self.kind {
            ConnectionKind::Memory => "/memory".to_string(),
            ConnectionKind::S3(s3) => {
                let mut query = vec![
                    format!("bucket={}", s3.bucket),
                    format!("access_key_id={}", s3.access_key_id),
                ];
                if !s3.region.is_empty() {
                    query.push(format!("region={}", s3.region));
                }
                if !s3.endpoint.is_empty() {
                    query.push(format!("endpoint={}", s3.endpoint));
                }
                if let Some(token) = &s3.session_token {
                    query.push(format!("session_token={token}"));
                }
                format!("/s3/{}?{}", self.name, query.join("&"))
            }
        }
    }

    /// Validate raw Add-mount form values into a `Connection`, with a specific,
    /// human-readable error naming the offending field. Pure (no I/O, no network) —
    /// the caller runs the live credential check separately.
    pub fn from_form(input: FormInput) -> Result<Connection, String> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err("Name is required.".to_string());
        }
        // `name` is a path component (mount point), a Keychain account, and an `-o`
        // value, so restrict it to a safe identifier — a space or slash would break
        // the `mount -o` parsing (that's the "Argument count N ≠ 2" error).
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(
                "Name can only contain letters, numbers, and . - _ (no spaces or slashes)."
                    .to_string(),
            );
        }

        // A custom mount folder (empty ⇒ the default `~/fskit-s3/<name>`). Whether it
        // exists and is empty is checked with I/O in `save_connection`, not here.
        let mount_point = {
            let m = input.mount_point.trim();
            (!m.is_empty()).then(|| m.to_string())
        };

        if !input.is_s3 {
            return Ok(Connection {
                name: name.to_string(),
                kind: ConnectionKind::Memory,
                save_secret_to_keychain: false,
                save_secret_to_disk: false,
                mount_on_launch: input.mount_on_launch,
                mount_point,
            });
        }

        let bucket = input.bucket.trim();
        let region = input.region.trim();
        let endpoint = input.endpoint.trim();
        let access_key_id = input.access_key_id.trim();
        let session_token = input.session_token.trim();
        let secret = input.secret.as_str(); // not trimmed — secrets may have edge whitespace

        if bucket.is_empty() {
            return Err("Bucket is required for an S3 connection.".to_string());
        }
        if access_key_id.is_empty() {
            return Err("Access Key ID is required for an S3 connection.".to_string());
        }
        if secret.is_empty() {
            return Err("Secret Access Key is required for an S3 connection.".to_string());
        }
        // Region is required. OpenDAL's S3 `build()` errors `ConfigInvalid` when the
        // region is empty *and* it can't be found in the ambient AWS environment
        // (`AWS_REGION`, `~/.aws/config`) — and it never auto-detects at build time
        // (that's a separate explicit `S3::detect_region()` network call we don't
        // make). We can't lean on that ambient fallback either: "Test & Save" runs in
        // this app, but the real mount runs in the *sandboxed* extension under fskitd,
        // which sees neither your shell env nor `~/.aws/config` — so a blank region
        // could pass the test and then fail the mount. Requiring it explicitly makes
        // the two agree. Real AWS needs the correct value; S3-compatible stores (MinIO,
        // RustFS, R2) ignore it but still need it non-empty (`us-east-1` is the usual
        // placeholder).
        if region.is_empty() {
            return Err("Region is required for S3 (e.g. us-east-1).".to_string());
        }
        if !endpoint.is_empty() {
            validate_endpoint(endpoint)?;
        }
        // These fields ride the source path's query string (`?k=v&k=v`), so they
        // must avoid its delimiters. (Endpoint keeps its `:` and `/` — those are
        // fine in a query value.)
        for (label, value) in [
            ("Bucket", bucket),
            ("Region", region),
            ("Access Key ID", access_key_id),
            ("Endpoint", endpoint),
            ("Session token", session_token),
        ] {
            if let Some(bad) = value.chars().find(|c| matches!(c, '?' | '&' | '=' | '#')) {
                return Err(format!("{label} can't contain '{bad}'."));
            }
        }
        // The secret rides `-o secret=…` unless it's saved to the Keychain (a
        // disk-stored secret is read back and passed via `-o` too), so in every
        // non-Keychain case it must avoid the `-o` list's comma delimiter.
        if !input.save_secret_to_keychain && secret.contains(',') {
            return Err(
                "Secret can't contain a comma unless it's saved to the Keychain.".to_string(),
            );
        }

        Ok(Connection {
            name: name.to_string(),
            kind: ConnectionKind::S3(S3Meta {
                bucket: bucket.to_string(),
                region: region.to_string(),
                endpoint: endpoint.to_string(),
                access_key_id: access_key_id.to_string(),
                session_token: (!session_token.is_empty()).then(|| session_token.to_string()),
            }),
            save_secret_to_keychain: input.save_secret_to_keychain,
            save_secret_to_disk: input.save_secret_to_disk,
            mount_on_launch: input.mount_on_launch,
            mount_point,
        })
    }
}

/// Raw values from the Add-mount form, validated by [`Connection::from_form`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct FormInput {
    pub name: String,
    pub is_s3: bool,
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret: String,
    pub session_token: String,
    /// On edit, keep the already-stored secret instead of using `secret` — set when
    /// the form's Secret field was left as its "a secret exists" placeholder (see
    /// `save_connection`). A *blank* `secret` is not this: it means an empty secret.
    pub keep_stored_secret: bool,
    pub save_secret_to_keychain: bool,
    /// Store the secret as a dev-only plaintext file on disk (see
    /// [`Connection::save_secret_to_disk`]). Insecure; for unsigned builds.
    pub save_secret_to_disk: bool,
    pub mount_on_launch: bool,
    /// A custom mount folder; empty ⇒ the default `~/fskit-s3/<name>` (see
    /// [`Connection::mount_point`]).
    pub mount_point: String,
}

/// An S3 endpoint must be an `http`/`https` URL with a host.
fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let url = url::Url::parse(endpoint).map_err(|_| {
        format!("Endpoint {endpoint:?} isn't a valid URL (e.g. https://s3.amazonaws.com).")
    })?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Endpoint scheme {other:?} must be http or https.")),
    }
    if url.host_str().unwrap_or("").is_empty() {
        return Err("Endpoint must include a host (e.g. https://s3.amazonaws.com).".to_string());
    }
    Ok(())
}

/// A set of connections keyed by unique name, persisted to an app-local file.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    connections: Vec<Connection>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Registry::default()
    }

    /// The starting set for a fresh install: just the built-in memory connection.
    pub fn with_defaults() -> Self {
        let mut r = Registry::new();
        // The name is unique in a fresh registry, so this never fails.
        let _ = r.add(Connection::memory());
        r
    }

    /// Load the persisted registry, falling back to [`Registry::with_defaults`]
    /// when the file is absent or unreadable (best-effort — never panics).
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    /// Persist the registry to the app-local config file.
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&config_path())
    }

    fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Registry::with_defaults();
        };
        match serde_json::from_str::<Vec<Connection>>(&text) {
            Ok(conns) => {
                let mut r = Registry::new();
                for c in conns {
                    let _ = r.add(c); // silently drop duplicate names
                }
                r
            }
            Err(_) => Registry::with_defaults(),
        }
    }

    fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.connections).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }

    /// All connections, in insertion order.
    pub fn list(&self) -> &[Connection] {
        &self.connections
    }

    /// The connection with this name, if any.
    pub fn get(&self, name: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.name == name)
    }

    /// Add a connection. Errors (without modifying the registry) if the name is
    /// already taken — names are the stable handle the UI addresses.
    pub fn add(&mut self, conn: Connection) -> Result<(), String> {
        if self.get(&conn.name).is_some() {
            return Err(format!("connection {:?} already exists", conn.name));
        }
        self.connections.push(conn);
        Ok(())
    }

    /// Remove the named connection; returns whether one was removed. Used by the
    /// Edit flow to replace an entry in place.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.connections.len();
        self.connections.retain(|c| c.name != name);
        self.connections.len() != before
    }
}

/// The default mount point for a connection named `name` (`~/fskit-s3/<name>`).
/// Standalone (not a method) so the contract can resolve it from a name alone —
/// the SwiftUI menu joins connections to the live mount list by this path.
pub fn default_mount_point_for(name: &str) -> PathBuf {
    base_dir().join(name)
}

/// The base directory for fskit-s3's mount points and resource dirs
/// (`~/fskit-s3`, or `/tmp/fskit-s3` if `$HOME` is unset). Exposed so the unmount
/// path can tell an app-managed default mount point (safe to clean up) from a
/// user-chosen folder (must be left alone).
pub(crate) fn base_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join("fskit-s3"),
        _ => PathBuf::from("/tmp/fskit-s3"),
    }
}

/// The app-local connections file (`~/Library/Application Support/fskit-s3/`).
fn config_path() -> PathBuf {
    let dir = match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => {
            PathBuf::from(home).join("Library/Application Support/fskit-s3")
        }
        _ => PathBuf::from("/tmp/fskit-s3"),
    };
    dir.join("connections.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s3_conn(name: &str) -> Connection {
        Connection {
            name: name.to_string(),
            kind: ConnectionKind::S3(S3Meta {
                bucket: "my-bucket".to_string(),
                region: "us-east-1".to_string(),
                endpoint: "http://localhost:9000".to_string(),
                access_key_id: "AKIA".to_string(),
                session_token: None,
            }),
            save_secret_to_keychain: true,
            save_secret_to_disk: false,
            mount_on_launch: false,
            mount_point: None,
        }
    }

    /// A valid S3 form (secret saved to Keychain), tweakable per test.
    fn s3_form() -> FormInput {
        FormInput {
            name: "photos".to_string(),
            is_s3: true,
            endpoint: "http://localhost:9000".to_string(),
            bucket: "my-bucket".to_string(),
            region: "us-east-1".to_string(),
            access_key_id: "AKIA".to_string(),
            secret: "s3cr3t".to_string(),
            session_token: String::new(),
            keep_stored_secret: false,
            save_secret_to_keychain: true,
            save_secret_to_disk: false,
            mount_on_launch: false,
            mount_point: String::new(),
        }
    }

    #[test]
    fn from_form_accepts_valid_memory_and_s3() {
        let mem = Connection::from_form(FormInput {
            name: "  local  ".to_string(),
            is_s3: false,
            ..s3_form()
        })
        .unwrap();
        assert_eq!(mem.name, "local"); // trimmed
        assert_eq!(mem.kind, ConnectionKind::Memory);

        let s3 = Connection::from_form(s3_form()).unwrap();
        assert!(s3.is_s3());
        assert!(s3.save_secret_to_keychain);
        assert!(!s3.save_secret_to_disk);
    }

    #[test]
    fn from_form_carries_save_secret_to_disk() {
        // The dev-only disk flag flows through unchanged, independent of Keychain.
        let disk = Connection::from_form(FormInput {
            save_secret_to_keychain: false,
            save_secret_to_disk: true,
            ..s3_form()
        })
        .unwrap();
        assert!(disk.save_secret_to_disk);
        assert!(!disk.save_secret_to_keychain);
    }

    #[test]
    fn mount_point_is_custom_or_default() {
        // No custom folder ⇒ the default `~/fskit-s3/<name>`.
        let def = s3_conn("photos");
        assert_eq!(def.mount_point(), default_mount_point_for("photos"));

        // A custom (non-empty) folder is used verbatim.
        let mut custom = s3_conn("photos");
        custom.mount_point = Some("/Volumes/backup".to_string());
        assert_eq!(custom.mount_point(), PathBuf::from("/Volumes/backup"));

        // An empty string is treated as "no custom folder".
        let mut blank = s3_conn("photos");
        blank.mount_point = Some(String::new());
        assert_eq!(blank.mount_point(), default_mount_point_for("photos"));

        // from_form maps a blank form field to None and a real one to Some.
        let from_blank = Connection::from_form(s3_form()).unwrap();
        assert_eq!(from_blank.mount_point, None);
        let from_custom = Connection::from_form(FormInput {
            mount_point: "  /Volumes/backup  ".to_string(),
            ..s3_form()
        })
        .unwrap();
        assert_eq!(from_custom.mount_point.as_deref(), Some("/Volumes/backup"));
    }

    #[test]
    fn from_form_rejects_bad_names() {
        let rejected = |name: &str| {
            Connection::from_form(FormInput {
                name: name.to_string(),
                ..s3_form()
            })
            .is_err()
        };
        assert!(rejected(""), "empty");
        for bad in ["a/b", "a,b", "a b", "a=b"] {
            assert!(rejected(bad), "{bad:?} should be rejected");
        }
        assert!(Connection::from_form(FormInput {
            name: "local-rustfs".to_string(),
            ..s3_form()
        })
        .is_ok());
    }

    #[test]
    fn from_form_requires_s3_essentials() {
        let missing = |f: fn(&mut FormInput)| {
            let mut form = s3_form();
            f(&mut form);
            Connection::from_form(form).unwrap_err()
        };
        assert!(missing(|f| f.bucket = String::new()).contains("Bucket"));
        assert!(missing(|f| f.access_key_id = String::new()).contains("Access Key"));
        assert!(missing(|f| f.secret = String::new()).contains("Secret"));
        assert!(missing(|f| f.region = String::new()).contains("Region"));
    }

    #[test]
    fn from_form_validates_endpoint_url() {
        let with_endpoint = |ep: &str| {
            Connection::from_form(FormInput {
                endpoint: ep.to_string(),
                ..s3_form()
            })
        };
        assert!(with_endpoint("").is_ok()); // empty ⇒ AWS default
        assert!(with_endpoint("https://s3.amazonaws.com").is_ok());
        assert!(with_endpoint("not a url")
            .unwrap_err()
            .contains("valid URL"));
        assert!(with_endpoint("ftp://host").unwrap_err().contains("http"));
    }

    #[test]
    fn from_form_rejects_query_delimiters_in_path_fields() {
        // Config fields ride the source path's query, so `?&=#` are rejected.
        for bad in ["a&b", "a=b", "a?b", "a#b"] {
            assert!(
                Connection::from_form(FormInput {
                    bucket: bad.to_string(),
                    ..s3_form()
                })
                .is_err(),
                "bucket {bad:?} should be rejected"
            );
        }
        // A typical endpoint (with `:` and `/`) is fine — those aren't delimiters.
        assert!(Connection::from_form(FormInput {
            endpoint: "http://localhost:9000".to_string(),
            ..s3_form()
        })
        .is_ok());
        // The secret still rides `-o secret=…`, so a comma is fine only via Keychain.
        assert!(Connection::from_form(FormInput {
            secret: "se,cret".to_string(),
            save_secret_to_keychain: true,
            ..s3_form()
        })
        .is_ok());
        assert!(Connection::from_form(FormInput {
            secret: "se,cret".to_string(),
            save_secret_to_keychain: false,
            ..s3_form()
        })
        .unwrap_err()
        .contains("comma"));
    }

    #[test]
    fn memory_connection_is_named_memory() {
        let c = Connection::memory();
        assert_eq!(c.name, "memory");
        assert_eq!(c.kind, ConnectionKind::Memory);
        assert!(!c.is_s3());
    }

    #[test]
    fn memory_source_path_is_just_memory() {
        assert_eq!(Connection::memory().source_path(), "/memory");
    }

    #[test]
    fn s3_source_path_carries_config_but_never_the_secret() {
        let path = s3_conn("photos").source_path();
        // `/s3/<name>?<query>`.
        assert!(path.starts_with("/s3/photos?"), "path was {path}");
        for kv in [
            "bucket=my-bucket",
            "access_key_id=AKIA",
            "region=us-east-1",
            "endpoint=http://localhost:9000",
        ] {
            assert!(path.contains(kv), "{kv:?} missing from {path}");
        }
        assert!(!path.contains("secret"), "the secret is never in the path");
    }

    #[test]
    fn s3_source_path_skips_empty_optional_fields() {
        let mut c = s3_conn("x");
        if let ConnectionKind::S3(m) = &mut c.kind {
            m.region = String::new();
            m.endpoint = String::new();
        }
        let path = c.source_path();
        assert!(!path.contains("region=") && !path.contains("endpoint="));
    }

    #[test]
    fn connection_serde_roundtrip() {
        let c = s3_conn("photos");
        let json = serde_json::to_string(&c).unwrap();
        // The secret access key has no field on the struct, so it can never be
        // serialized (the `save_secret_to_keychain` flag name is unrelated).
        assert!(!json.contains("secret_access_key"));
        let back: Connection = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn registry_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fskit-s3-test-{}", std::process::id()));
        let path = dir.join("connections.json");
        let mut r = Registry::with_defaults();
        r.add(s3_conn("photos")).unwrap();
        r.save_to(&path).unwrap();

        let loaded = Registry::load_from(&path);
        assert_eq!(loaded.list().len(), 2);
        assert!(loaded.get("memory").is_some());
        assert!(loaded.get("photos").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_falls_back_to_defaults() {
        let r = Registry::load_from(Path::new("/nonexistent/fskit-s3/connections.json"));
        assert_eq!(r.list().len(), 1);
        assert!(r.get("memory").is_some());
    }

    #[test]
    fn defaults_hold_only_memory() {
        let r = Registry::with_defaults();
        assert_eq!(r.list().len(), 1);
        assert!(r.get("memory").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn add_rejects_duplicate_names() {
        let mut r = Registry::with_defaults();
        let err = r.add(Connection::memory()).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(r.list().len(), 1);
    }

    #[test]
    fn add_then_remove_roundtrips() {
        let mut r = Registry::new();
        assert!(r.add(s3_conn("photos")).is_ok());
        assert!(r.get("photos").is_some());
        assert!(r.remove("photos"));
        assert!(r.get("photos").is_none());
        assert!(!r.remove("photos"), "removing a missing name reports false");
    }
}
