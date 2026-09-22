use std::{
    fmt,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Stat,
    Read,
    Write,
    SetMeta,
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stat => write!(f, "stat"),
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
            Self::SetMeta => write!(f, "set metadata"),
        }
    }
}

#[derive(Debug)]
pub enum FsError {
    Denied { path: PathBuf, op: Op },
    NotFound(PathBuf),
    Other(io::Error),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied { path, op } => {
                write!(f, "permission denied: {} {}", op, path.display())
            }
            Self::NotFound(path) => write!(f, "no such file: {}", path.display()),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Other(e) => Some(e),
            _ => None,
        }
    }
}

pub fn map_io(path: &Path, op: Op, e: io::Error) -> FsError {
    match e.kind() {
        io::ErrorKind::NotFound => FsError::NotFound(path.to_path_buf()),
        io::ErrorKind::PermissionDenied => FsError::Denied {
            path: path.to_path_buf(),
            op,
        },
        _ => FsError::Other(e),
    }
}

// ---------- metadata ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => write!(f, "file"),
            Self::Dir => write!(f, "directory"),
            Self::Symlink => write!(f, "symlink"),
            Self::Other => write!(f, "other"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub size: u64,
    pub kind: Kind,
    pub mode: Option<u32>,
    pub mtime_ns: Option<i64>,
}

// lstat: never follows symlinks — callers decide if following is safe
pub fn metadata(path: &Path) -> Result<FileMeta, FsError> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| map_io(path, Op::Stat, e))?;
    let kind = if meta.is_symlink() {
        Kind::Symlink
    } else if meta.is_file() {
        Kind::File
    } else if meta.is_dir() {
        Kind::Dir
    } else {
        Kind::Other
    };

    Ok(FileMeta {
        size: meta.len(),
        kind,
        mode: unix_mode(&meta),
        mtime_ns: mtime_ns(&meta),
    })
}

#[cfg(unix)]
fn unix_mode(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn unix_mode(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

fn mtime_ns(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
}

// preallocate avoids fragmentation and ensures space before concurrent pwrite callers begin
pub fn preallocate(f: &File, size: u64) -> Result<(), FsError> {
    f.set_len(size).map_err(FsError::Other)
}

// failures are returned; call-site decides whether to warn or abort
pub fn apply_metadata(
    path: &Path,
    mode: Option<u32>,
    mtime_ns: Option<i64>,
) -> Result<(), FsError> {
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(m))
            .map_err(|e| map_io(path, Op::SetMeta, e))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    if let Some(ns) = mtime_ns
        && let Some(st) = system_time_from_ns(ns)
    {
        File::options()
            .read(true)
            .open(path)
            .map_err(|e| map_io(path, Op::SetMeta, e))?
            .set_modified(st)
            .map_err(|e| map_io(path, Op::SetMeta, e))?;
    }
    Ok(())
}

pub fn system_time_from_ns(ns: i64) -> Option<SystemTime> {
    if ns >= 0 {
        Some(UNIX_EPOCH + Duration::from_nanos(ns as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_nanos(ns.unsigned_abs()))
    }
}

// ---------- positional I/O ----------

// surfaces write errors that would otherwise only appear on close
pub fn sync(f: &File) -> Result<(), FsError> {
    f.sync_all().map_err(FsError::Other)
}

pub fn read_at(f: &File, buf: &mut [u8], offset: u64) -> Result<usize, FsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_at(buf, offset).map_err(FsError::Other)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        f.seek_read(buf, offset).map_err(FsError::Other)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (f, buf, offset);
        Err(FsError::Other(io::Error::new(
            io::ErrorKind::Unsupported,
            "positional reads need unix or windows",
        )))
    }
}

pub fn write_at(f: &File, buf: &[u8], offset: u64) -> Result<usize, FsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.write_at(buf, offset).map_err(FsError::Other)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        f.seek_write(buf, offset).map_err(FsError::Other)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (f, buf, offset);
        Err(FsError::Other(io::Error::new(
            io::ErrorKind::Unsupported,
            "positional writes need unix or windows",
        )))
    }
}

// ---------- path helpers ----------

// windows/macOS would silently merge case-variant names — hard-error instead
pub fn find_case_collision<S: AsRef<str>>(paths: &[S]) -> Option<(String, String)> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, p) in paths.iter().enumerate() {
        let folded = p.as_ref().to_lowercase();
        if let Some(prev_i) = seen.insert(folded, i) {
            return Some((paths[prev_i].as_ref().to_string(), p.as_ref().to_string()));
        }
    }
    None
}

// None for non-UTF-8, absolute, .., or empty paths — caller skips
pub fn rel_to_unix(rel: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- find_case_collision ---

    #[test]
    fn no_collision_on_empty_input() {
        let paths: Vec<&str> = vec![];
        assert!(find_case_collision(&paths).is_none());
    }

    #[test]
    fn no_collision_on_distinct_names() {
        let paths = vec!["foo/bar.txt", "foo/baz.txt", "other/qux.rs"];
        assert!(find_case_collision(&paths).is_none());
    }

    #[test]
    fn collision_detected_for_exact_duplicate() {
        let paths = vec!["foo/bar.txt", "foo/bar.txt"];
        assert!(find_case_collision(&paths).is_some());
    }

    #[test]
    fn collision_detected_for_case_variant() {
        let paths = vec!["foo/Bar.txt", "foo/bar.txt"];
        assert!(find_case_collision(&paths).is_some());
    }

    #[test]
    fn collision_works_with_owned_strings() {
        let paths: Vec<String> = vec!["Readme.md".to_string(), "README.md".to_string()];
        assert!(find_case_collision(&paths).is_some());
    }

    // --- rel_to_unix ---

    #[test]
    fn simple_filename_converts_unchanged() {
        assert_eq!(
            rel_to_unix(Path::new("foo.txt")),
            Some("foo.txt".to_string())
        );
    }

    #[test]
    fn nested_path_uses_forward_slashes() {
        assert_eq!(
            rel_to_unix(Path::new("a/b/c.txt")),
            Some("a/b/c.txt".to_string())
        );
    }

    #[test]
    fn dotdot_component_returns_none() {
        assert_eq!(rel_to_unix(Path::new("a/../b.txt")), None);
    }

    #[test]
    fn absolute_path_returns_none() {
        assert_eq!(rel_to_unix(Path::new("/etc/passwd")), None);
    }

    #[test]
    fn empty_path_returns_none() {
        assert_eq!(rel_to_unix(Path::new("")), None);
    }

    // --- system_time_from_ns ---

    #[test]
    fn positive_ns_is_after_epoch() {
        let t = system_time_from_ns(1_000_000_000);
        assert!(t.is_some());
        assert!(t.unwrap() > UNIX_EPOCH);
    }

    #[test]
    fn zero_ns_is_the_epoch() {
        assert_eq!(system_time_from_ns(0), Some(UNIX_EPOCH));
    }

    #[test]
    fn negative_ns_is_before_epoch() {
        let t = system_time_from_ns(-1_000_000_000);
        assert!(t.is_some());
        assert!(t.unwrap() < UNIX_EPOCH);
    }
}
