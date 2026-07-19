//! Hiding macOS's volume-metadata litter from the object store.
//!
//! macOS treats an FSKit mount as a genuine local volume, so its daemons write
//! bookkeeping to the volume: `.fseventsd` (FSEvents change logs), `.Spotlight-V100`
//! (the Spotlight index), `.Trashes`, `.TemporaryItems`, `.DS_Store` (Finder), and
//! `._*` AppleDouble sidecars (resource forks / xattrs). None of it belongs in a
//! user's bucket, and left alone it accumulates — the bucket that prompted this had
//! `.fseventsd`, `.Trashes`, `.TemporaryItems`, and `._*` files strewn through it.
//!
//! We can't stop macOS from *trying*, but the ext is the seam every request passes
//! through, so it refuses to create these names and hides any that already leaked:
//! [`is_hidden`] gates `lookup` (→ ENOENT), `enumerate` (skip), and `createItem`
//! (→ EPERM), so they never reach the [`StorageBackend`](fskit_s3_core::StorageBackend).
//!
//! Matching is on the **basename**, at every directory level: a `.DS_Store` or
//! `._foo` in any subfolder is caught, and hiding the top-level `.fseventsd` keeps
//! its whole subtree from ever being created.
//!
//! Deliberately **not** hidden: editor scratch files (vim's `4913` write probes,
//! `.swp`/`.swx` swap files, `~` backups) and atomic-save temps (`*.sb-*`). Those
//! belong to a legitimate write flow and are cleaned up by the tool that made them
//! — hiding them would break the editor. (Their past leaks were a delete bug, fixed
//! separately, not litter to paper over here.)

/// Exact names macOS generates for its own volume bookkeeping.
const HIDDEN_EXACT: &[&str] = &[
    ".DS_Store",               // Finder per-directory metadata
    ".Spotlight-V100",         // Spotlight index
    ".fseventsd",              // FSEvents change log
    ".Trashes",                // per-volume trash
    ".TemporaryItems",         // scratch dir macOS creates on a volume
    ".DocumentRevisions-V100", // version storage
    ".apdisk",                 // Apple disk metadata
    ".VolumeIcon.icns",        // custom volume icon
];

/// Whether `name` (a single path component) is macOS volume litter that must never
/// reach the backend. Callers pass the basename, never a full path.
pub fn is_hidden(name: &str) -> bool {
    // AppleDouble resource-fork / xattr sidecars: `._<anything>`. The `._` prefix
    // is macOS's namespace, so even a bare `._` is litter, but a name that merely
    // *contains* `._` later (e.g. `note._old`) is a real file.
    name.starts_with("._") || HIDDEN_EXACT.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::is_hidden;

    #[test]
    fn hides_macos_metadata() {
        for n in [
            ".DS_Store",
            ".Spotlight-V100",
            ".fseventsd",
            ".Trashes",
            ".TemporaryItems",
            ".DocumentRevisions-V100",
            ".apdisk",
            ".VolumeIcon.icns",
        ] {
            assert!(is_hidden(n), "{n} should be hidden");
        }
    }

    #[test]
    fn hides_appledouble_sidecars() {
        assert!(is_hidden("._hello.txt")); // sidecar of hello.txt
        assert!(is_hidden("._")); // the bare prefix is still macOS's
        assert!(is_hidden("._.DS_Store")); // sidecar of a hidden file
    }

    #[test]
    fn keeps_real_files_and_editor_artifacts() {
        // Regular files and dotfiles stay visible, and so do editor scratch files:
        // the delete path cleans those up, and hiding them would break the editor.
        for n in [
            "hello.txt",
            "photos",
            ".gitignore",     // a real dotfile the user wants
            ".env",           // ditto
            "4913",           // vim write probe
            ".hello.txt.swp", // vim swap file
            "test~",          // vim backup
            "note._old",      // `._` not at the start → a real file
            "doc.sb-abc123",  // atomic-save temp
        ] {
            assert!(!is_hidden(n), "{n} should stay visible");
        }
    }
}
