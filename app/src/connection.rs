//! Configured connections — the things you can mount.
//!
//! A [`Connection`] names a storage endpoint; a [`Registry`] holds the set of
//! them. Both are pure data with no I/O, so they unit-test trivially and are
//! kept separate from the app's AppKit/`unsafe` layer.

use std::path::PathBuf;

/// A configured storage connection: an identity plus the backend it maps to.
///
/// Mounting a connection hands its [`source_dir`](Connection::source_dir) to the
/// extension as the `mount` resource argument. For [`ConnectionKind::Demo`] the
/// extension ignores that path (it serves a fixed in-memory tree), but `mount`
/// still requires a path, and giving each connection a distinct one keeps their
/// FSKit container identities separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    pub kind: ConnectionKind,
}

/// Which backend a connection is served by.
///
/// Only the in-memory demo exists today; the S3/WebDAV/… variants arrive with the
/// config + Keychain milestone. Keeping this an enum means the front-ends match
/// on it once and gain the new kinds without structural change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionKind {
    /// The credential-free in-memory tree the extension currently serves.
    Demo,
    // S3 { endpoint: String, bucket: String, region: String } — next milestone.
}

impl ConnectionKind {
    /// A short human label, e.g. for a CLI listing or a menu subtitle.
    pub fn label(&self) -> &'static str {
        match self {
            ConnectionKind::Demo => "in-memory demo",
        }
    }
}

impl Connection {
    /// The built-in, credential-free demo connection.
    pub fn demo() -> Self {
        Connection {
            name: "demo".to_string(),
            kind: ConnectionKind::Demo,
        }
    }

    /// The resource directory handed to `mount` for this connection.
    ///
    /// A hidden per-connection directory under [`base_dir`]; created on demand by
    /// [`mount`](crate::mount). Distinct per connection so container identities
    /// don't collide.
    pub fn source_dir(&self) -> PathBuf {
        base_dir().join(".sources").join(&self.name)
    }

    /// Where this connection is mounted by default (`~/fskit-s3/<name>`).
    ///
    /// A visible directory in the user's home so the mount is easy to find; used
    /// when the caller doesn't name a mount point.
    pub fn default_mount_point(&self) -> PathBuf {
        base_dir().join(&self.name)
    }

    /// The `-o key=value` options handed to `mount` for this connection.
    ///
    /// This is the whole point of the "no bespoke CLI" design: a connection's
    /// configuration travels as **mount options**, so mounting is just the system
    /// `mount` tool (the app builds the option string; a human could type the same
    /// command). The demo needs none. S3 connections will carry `endpoint`,
    /// `bucket`, `region`, … here — never the secret, which the extension fetches
    /// from the Keychain by the connection's identity. Reading these options back
    /// in the extension's `loadResource:options:` is part of the S3 milestone.
    pub fn mount_options(&self) -> Vec<(String, String)> {
        match &self.kind {
            ConnectionKind::Demo => Vec::new(),
        }
    }
}

/// An in-memory set of connections, keyed by unique name.
///
/// Not yet persisted: [`Registry::with_defaults`] rebuilds the same starting set
/// (just the demo) on each process. [`add`](Registry::add)/[`remove`](Registry::remove)
/// already model mutation so the front-ends are written against the final shape;
/// wiring them to a config file + Keychain is the next milestone.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    connections: Vec<Connection>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Registry::default()
    }

    /// The registry every front-end starts from: the built-in demo connection.
    pub fn with_defaults() -> Self {
        let mut r = Registry::new();
        // The demo name is unique in a fresh registry, so this never fails.
        let _ = r.add(Connection::demo());
        r
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
    /// already taken — names are the stable handle the front-ends address.
    pub fn add(&mut self, conn: Connection) -> Result<(), String> {
        if self.get(&conn.name).is_some() {
            return Err(format!("connection {:?} already exists", conn.name));
        }
        self.connections.push(conn);
        Ok(())
    }

    /// Remove the named connection; returns whether one was removed.
    ///
    /// Part of the registry's mutation API for the upcoming connection-config UI
    /// (add/edit/remove); not wired to a menu action yet, hence unused today.
    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_connection_is_named_demo() {
        let c = Connection::demo();
        assert_eq!(c.name, "demo");
        assert_eq!(c.kind, ConnectionKind::Demo);
        assert_eq!(c.kind.label(), "in-memory demo");
    }

    #[test]
    fn source_and_mount_paths_are_distinct_and_named() {
        let c = Connection::demo();
        let mnt = c.default_mount_point();
        let src = c.source_dir();
        assert!(mnt.ends_with("demo"), "mount point ends with the name");
        assert!(src.ends_with("demo"), "source dir ends with the name");
        assert_ne!(mnt, src, "a mount point is never its own resource dir");
        assert!(
            src.to_string_lossy().contains(".sources"),
            "sources live in a hidden subdir"
        );
    }

    #[test]
    fn demo_needs_no_mount_options() {
        assert!(Connection::demo().mount_options().is_empty());
    }

    #[test]
    fn defaults_hold_only_the_demo() {
        let r = Registry::with_defaults();
        assert_eq!(r.list().len(), 1);
        assert!(r.get("demo").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn add_rejects_duplicate_names() {
        let mut r = Registry::with_defaults();
        let err = r.add(Connection::demo()).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(
            r.list().len(),
            1,
            "a rejected add doesn't grow the registry"
        );
    }

    #[test]
    fn add_then_remove_roundtrips() {
        let mut r = Registry::new();
        let c = Connection {
            name: "photos".to_string(),
            kind: ConnectionKind::Demo,
        };
        assert!(r.add(c).is_ok());
        assert!(r.get("photos").is_some());
        assert!(r.remove("photos"));
        assert!(r.get("photos").is_none());
        assert!(!r.remove("photos"), "removing a missing name reports false");
    }
}
