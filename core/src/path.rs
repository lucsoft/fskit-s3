//! Absolute-path helpers shared by every backend.
//!
//! One canonical form so backends never each reinvent basename/parent handling:
//! `"/"` for the root, otherwise a leading `/` and no trailing `/`, with `.` and
//! `..` (and empty components from doubled slashes) collapsed.

/// Normalize an absolute path into canonical form.
///
/// `..` pops the previous component; a `..` at the root is clamped (it cannot
/// escape above `/`). The result always begins with `/`.
///
/// ```
/// # use fskit_s3_core::path::normalize;
/// assert_eq!(normalize("/a/b/../c/"), "/a/c");
/// assert_eq!(normalize("//a//./b"), "/a/b");
/// assert_eq!(normalize("/../.."), "/");
/// assert_eq!(normalize(""), "/");
/// ```
pub fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    if out.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", out.join("/"))
    }
}

/// Basename of a normalized path. The root's basename is `""`.
///
/// ```
/// # use fskit_s3_core::path::basename;
/// assert_eq!(basename("/a/b.txt"), "b.txt");
/// assert_eq!(basename("/"), "");
/// ```
pub fn basename(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((_, name)) => name,
        None => path,
    }
}

/// Parent of a normalized path. The root's parent is the root.
///
/// ```
/// # use fskit_s3_core::path::parent;
/// assert_eq!(parent("/a/b.txt"), "/a");
/// assert_eq!(parent("/a"), "/");
/// assert_eq!(parent("/"), "/");
/// ```
pub fn parent(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((head, _)) => head,
        None => "/",
    }
}

/// Convert a normalized absolute path into an S3-style object key: no leading
/// slash, and (for a directory prefix) a single trailing slash when `dir` is set.
///
/// ```
/// # use fskit_s3_core::path::to_key;
/// assert_eq!(to_key("/a/b.txt", false), "a/b.txt");
/// assert_eq!(to_key("/a/b", true), "a/b/");
/// assert_eq!(to_key("/", true), "");
/// ```
pub fn to_key(path: &str, dir: bool) -> String {
    let stripped = path.strip_prefix('/').unwrap_or(path);
    if stripped.is_empty() {
        String::new()
    } else if dir {
        format!("{stripped}/")
    } else {
        stripped.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_and_clamps() {
        assert_eq!(normalize("/a/b/../c/"), "/a/c");
        assert_eq!(normalize("//a//./b"), "/a/b");
        assert_eq!(normalize("/../.."), "/");
        assert_eq!(normalize(""), "/");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("relative/path"), "/relative/path");
    }

    #[test]
    fn basename_and_parent() {
        assert_eq!(basename("/a/b.txt"), "b.txt");
        assert_eq!(basename("/"), "");
        assert_eq!(parent("/a/b.txt"), "/a");
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/"), "/");
    }

    #[test]
    fn keys() {
        assert_eq!(to_key("/a/b.txt", false), "a/b.txt");
        assert_eq!(to_key("/a/b", true), "a/b/");
        assert_eq!(to_key("/", true), "");
        assert_eq!(to_key("/", false), "");
    }
}
