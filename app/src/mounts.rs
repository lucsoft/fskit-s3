//! Active mounts — enumerating them and realising/removing them.
//!
//! Everything here shells out to the system tools (`/sbin/mount`,
//! `/usr/sbin/diskutil`) so it stays pure Rust and fully unit-tested: [`parse`]
//! is exercised directly, and the side-effecting calls are thin wrappers whose
//! failure paths return the tool's own stderr.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::connection::Connection;

/// The filesystem type FSKit reports for our volumes (the module's `FSShortName`).
pub const FS_TYPE: &str = "fskit-s3";

/// One row of `/sbin/mount`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Mount {
    pub device: String,
    pub mount_point: String,
    pub fs_type: String,
}

/// Parse `/sbin/mount` output. Each line looks like:
/// `/dev/disk1s1 on / (apfs, local, journaled)`.
pub fn parse(output: &str) -> Vec<Mount> {
    output.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Mount> {
    let (device, rest) = line.split_once(" on ")?;
    // The mount point may contain spaces, so split on the LAST " (".
    let (path, tail) = rest.rsplit_once(" (")?;
    let fs_type = tail.split(',').next()?.trim_end_matches(')').trim();
    Some(Mount {
        device: device.trim().to_string(),
        mount_point: path.trim().to_string(),
        fs_type: fs_type.to_string(),
    })
}

/// All current mounts (best-effort: empty if `mount` can't be run).
pub fn list() -> Vec<Mount> {
    match Command::new("/sbin/mount").output() {
        Ok(out) => parse(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => Vec::new(),
    }
}

/// Mounts served by this filesystem (type contains `fskit`).
///
/// FSKit reports the module's short name as the fs type; we match on `fskit`
/// so both `fskit-s3` and future variants show up.
pub fn list_fskit() -> Vec<Mount> {
    list()
        .into_iter()
        .filter(|m| m.fs_type.contains("fskit"))
        .collect()
}

/// Mount a connection at `mount_point`.
///
/// Ensures the mount point exists, then runs
/// `mount -F -t fskit-s3 [-o secret=…] <source> <mount_point>`, where `<source>`
/// is the connection's [`source_path`](Connection::source_path) — a self-describing
/// path (`/memory` or `/s3/<name>?…`) that carries the whole config, so the
/// extension resolves it at `loadResource`. The source needn't exist on disk. When
/// `secret` is supplied it's passed as `-o secret=…` — the **insecure** path, only
/// for connections whose secret isn't in the Keychain (the ext prefers
/// `Keychain[name]`). Requires the FSKit extension to be installed and enabled; if
/// it isn't, `mount` fails and its stderr is returned unchanged.
pub fn mount(conn: &Connection, mount_point: &Path, secret: Option<&str>) -> Result<(), String> {
    fs::create_dir_all(mount_point)
        .map_err(|e| format!("create mount point {}: {e}", mount_point.display()))?;

    // The secret is the only `-o` option left; all other config rides the source path.
    let mut options = Vec::new();
    if let Some(secret) = secret {
        options.push(("secret".to_string(), secret.to_string()));
    }

    let mut cmd = Command::new("/sbin/mount");
    cmd.args(["-F", "-t", FS_TYPE]);
    if let Some(opts) = format_options(&options) {
        cmd.arg("-o").arg(opts);
    }
    let out = cmd
        .arg(conn.source_path())
        .arg(mount_point)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(stderr_or_status(&out))
    }
}

/// Render `key=value` pairs into a `mount -o` comma string, or `None` if empty.
fn format_options(opts: &[(String, String)]) -> Option<String> {
    if opts.is_empty() {
        return None;
    }
    let joined = opts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    Some(joined)
}

/// Unmount a path via `diskutil unmount`. Returns the tool's stderr on failure.
pub fn unmount(mount_point: &str) -> Result<(), String> {
    let out = Command::new("/usr/sbin/diskutil")
        .arg("unmount")
        .arg(mount_point)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(stderr_or_status(&out));
    }
    // The volume is detached now, so the mount point is a plain empty directory the
    // app created at mount time. Remove it so an unmounted connection leaves no
    // stray folder behind. Best-effort and empty-only (`remove_dir`, not
    // `remove_dir_all`): if anything is there, or it's still busy, leave it be — a
    // later mount recreates it anyway.
    let _ = fs::remove_dir(mount_point);
    Ok(())
}

/// The stderr of a failed command, trimmed; falls back to the exit status when
/// the tool wrote nothing (so the error is never an empty string).
fn stderr_or_status(out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if err.is_empty() {
        format!("exited with {}", out.status)
    } else {
        err
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_mount_output() {
        let sample = "\
/dev/disk1s1s1 on / (apfs, sealed, local, read-only, journaled)
/dev/disk1s2 on /System/Volumes/Data (apfs, local, journaled)
map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)
fskit-s3://test-bucket on /Volumes/My Bucket (fskit-s3, local, nodev)";
        let mounts = parse(sample);
        assert_eq!(mounts.len(), 4);
        assert_eq!(mounts[0].mount_point, "/");
        assert_eq!(mounts[0].fs_type, "apfs");
        // Mount point with a space is preserved.
        assert_eq!(mounts[3].mount_point, "/Volumes/My Bucket");
        assert_eq!(mounts[3].fs_type, "fskit-s3");
    }

    #[test]
    fn filters_to_fskit() {
        let sample = "\
/dev/disk1s1 on / (apfs, local)
fskit-s3://b on /Volumes/b (fskit-s3, local)";
        let fskit = parse(sample)
            .into_iter()
            .filter(|m| m.fs_type.contains("fskit"))
            .collect::<Vec<_>>();
        assert_eq!(fskit.len(), 1);
        assert_eq!(fskit[0].mount_point, "/Volumes/b");
    }

    #[test]
    fn ignores_garbage_lines() {
        assert!(parse("not a mount line\n\n").is_empty());
    }

    #[test]
    fn options_render_as_comma_string_or_none() {
        assert_eq!(format_options(&[]), None);
        let opts = vec![
            ("endpoint".to_string(), "http://x".to_string()),
            ("bucket".to_string(), "b".to_string()),
        ];
        assert_eq!(
            format_options(&opts).as_deref(),
            Some("endpoint=http://x,bucket=b")
        );
    }

    #[test]
    fn stderr_falls_back_to_status_when_empty() {
        use std::os::unix::process::ExitStatusExt;
        let out = std::process::Output {
            status: std::process::ExitStatus::from_raw(256), // exit code 1
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(stderr_or_status(&out).starts_with("exited with"));
    }
}
