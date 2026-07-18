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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub name: String,
    pub kind: ConnectionKind,
    /// Whether the secret (S3 only) is stored in the Keychain. When false the
    /// secret isn't persisted and a mount must supply it (prompt or `-o secret`).
    #[serde(default)]
    pub save_secret_to_keychain: bool,
    /// Mount this connection automatically when the app launches.
    #[serde(default)]
    pub mount_on_launch: bool,
}

/// Which backend a connection is served by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionKind {
    /// The credential-free in-memory demo tree the extension serves.
    Memory,
    /// An S3 (or S3-compatible) bucket. Non-secret config only.
    S3(S3Meta),
}

/// Non-secret S3 connection config (mirrors `fskit_s3_backend::S3Config` minus the
/// `secret_access_key`, which never touches this struct or the config file).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct S3Meta {
    pub bucket: String,
    pub region: String,
    /// Custom endpoint for S3-compatible stores (MinIO, R2, RustFS). Empty ⇒ AWS.
    pub endpoint: String,
    pub access_key_id: String,
    pub session_token: Option<String>,
}

impl ConnectionKind {
    /// A short human label for a menu subtitle.
    pub fn label(&self) -> &'static str {
        match self {
            ConnectionKind::Memory => "in-memory demo",
            ConnectionKind::S3(_) => "S3",
        }
    }
}

impl Connection {
    /// The built-in, credential-free in-memory connection.
    pub fn memory() -> Self {
        Connection {
            name: "memory".to_string(),
            kind: ConnectionKind::Memory,
            save_secret_to_keychain: false,
            mount_on_launch: false,
        }
    }

    /// Whether this is an S3 connection (needs a secret to mount).
    pub fn is_s3(&self) -> bool {
        matches!(self.kind, ConnectionKind::S3(_))
    }

    /// The resource directory handed to `mount` for this connection.
    ///
    /// A hidden per-connection directory under [`base_dir`]; created on demand by
    /// [`crate::mounts::mount`]. Distinct per connection so FSKit container
    /// identities don't collide.
    pub fn source_dir(&self) -> PathBuf {
        base_dir().join(".sources").join(&self.name)
    }

    /// Where this connection is mounted by default (`~/fskit-s3/<name>`).
    pub fn default_mount_point(&self) -> PathBuf {
        base_dir().join(&self.name)
    }

    /// The `-o key=value` options handed to `mount` for this connection — its
    /// non-secret config. Always carries an explicit `type` (`memory` | `s3`) so
    /// the extension dispatches unambiguously and **errors** on a mount with no
    /// `type` rather than silently serving the demo. For S3, `name` identifies the
    /// connection (the extension's Keychain account); the demo needs no `name`. The
    /// **secret** is never included; [`crate::mounts::mount`] appends `secret=…` for
    /// the insecure path.
    ///
    /// Values must not contain commas (the `-o` list delimiter) — true for
    /// endpoints/buckets/regions/keys; secrets go through the Keychain instead.
    pub fn mount_options(&self) -> Vec<(String, String)> {
        match &self.kind {
            ConnectionKind::Memory => vec![("type".to_string(), "memory".to_string())],
            ConnectionKind::S3(s3) => {
                let mut opts = vec![
                    ("type".to_string(), "s3".to_string()),
                    ("name".to_string(), self.name.clone()),
                    ("bucket".to_string(), s3.bucket.clone()),
                    ("access_key_id".to_string(), s3.access_key_id.clone()),
                ];
                if !s3.region.is_empty() {
                    opts.push(("region".to_string(), s3.region.clone()));
                }
                if !s3.endpoint.is_empty() {
                    opts.push(("endpoint".to_string(), s3.endpoint.clone()));
                }
                if let Some(token) = &s3.session_token {
                    opts.push(("session_token".to_string(), token.clone()));
                }
                opts
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

        if !input.is_s3 {
            return Ok(Connection {
                name: name.to_string(),
                kind: ConnectionKind::Memory,
                save_secret_to_keychain: false,
                mount_on_launch: input.mount_on_launch,
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
        // OpenDAL requires a region; without one the backend fails to build with a
        // terse "ConfigInvalid". Real AWS needs the correct region; most S3-compatible
        // stores (MinIO, RustFS, R2) ignore the value but still need it non-empty.
        if region.is_empty() {
            return Err("Region is required for S3 (e.g. us-east-1).".to_string());
        }
        if !endpoint.is_empty() {
            validate_endpoint(endpoint)?;
        }
        // Everything that rides the `-o` option string must avoid its comma delimiter.
        for (label, value) in [
            ("Bucket", bucket),
            ("Region", region),
            ("Access Key ID", access_key_id),
            ("Session token", session_token),
        ] {
            if value.contains(',') {
                return Err(format!("{label} can't contain a comma."));
            }
        }
        // The secret rides `-o` only when it isn't saved to the Keychain.
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
            mount_on_launch: input.mount_on_launch,
        })
    }
}

/// Raw values from the Add-mount form, validated by [`Connection::from_form`].
pub struct FormInput {
    pub name: String,
    pub is_s3: bool,
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret: String,
    pub session_token: String,
    pub save_secret_to_keychain: bool,
    pub mount_on_launch: bool,
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

/// The base directory for fskit-s3's mount points and resource dirs
/// (`~/fskit-s3`, or `/tmp/fskit-s3` if `$HOME` is unset).
fn base_dir() -> PathBuf {
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
            mount_on_launch: false,
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
            save_secret_to_keychain: true,
            mount_on_launch: false,
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
    fn from_form_rejects_commas_in_o_fields() {
        assert!(Connection::from_form(FormInput {
            bucket: "a,b".to_string(),
            ..s3_form()
        })
        .unwrap_err()
        .contains("comma"));
        // A secret with a comma is fine when it goes to the Keychain, not `-o`.
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
        assert_eq!(c.kind.label(), "in-memory demo");
        assert!(!c.is_s3());
    }

    #[test]
    fn memory_mount_options_are_just_the_type() {
        let opts = Connection::memory().mount_options();
        assert_eq!(opts, vec![("type".to_string(), "memory".to_string())]);
    }

    #[test]
    fn s3_mount_options_start_with_the_type() {
        let opts = s3_conn("photos").mount_options();
        assert_eq!(opts.first(), Some(&("type".to_string(), "s3".to_string())));
    }

    #[test]
    fn s3_mount_options_carry_config_but_never_the_secret() {
        let opts = s3_conn("photos").mount_options();
        let has = |k: &str| opts.iter().any(|(key, _)| key == k);
        assert!(has("name") && has("bucket") && has("access_key_id"));
        assert!(has("region") && has("endpoint"));
        assert!(!has("secret"), "the secret is never a mount option");
        assert_eq!(
            opts.iter()
                .find(|(k, _)| k == "name")
                .map(|(_, v)| v.as_str()),
            Some("photos")
        );
    }

    #[test]
    fn s3_mount_options_skip_empty_optional_fields() {
        let mut c = s3_conn("x");
        if let ConnectionKind::S3(m) = &mut c.kind {
            m.region = String::new();
            m.endpoint = String::new();
        }
        let opts = c.mount_options();
        assert!(!opts.iter().any(|(k, _)| k == "region" || k == "endpoint"));
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
