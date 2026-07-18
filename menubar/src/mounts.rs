//! Enumerate and manage mounts by shelling out to the system tools.
//!
//! Pure Rust, no `objc2` — so it's fully unit-tested and panic-free. The AppKit
//! layer (`main.rs`) only calls into here.

use std::process::Command;

/// One row of `/sbin/mount`.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Unmount a path via `diskutil unmount`. Returns the tool's stderr on failure.
pub fn unmount(mount_point: &str) -> Result<(), String> {
    let out = Command::new("/usr/sbin/diskutil")
        .arg("unmount")
        .arg(mount_point)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
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
}
