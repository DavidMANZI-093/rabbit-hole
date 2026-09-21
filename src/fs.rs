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

// Uses `lstat` behavior: symlinks are reported, never followed.
// Prevents directory traversal attacks where recursive walks expose
// sensitive files. Following must be an explicit decision inferred
// from a hard tree.
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

// `mode` is unix-only. `None` is returned on other platforms
// signaling omittion.
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

// Ensures concurrent `pwrite` allocations don't scatter causing fragmentation,
// fewer fs metadata inquiries, space availability, and true parallelism.
pub fn preallocate(f: &File, size: u64) -> Result<(), FsError> {
    f.set_len(size).map_err(FsError::Other)
}

// `mtime` failures and `chmod` failures return `Err`; leniency remains call-site's
// policy, not this layer's.
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
    let _ = mode; // inapplicable: no mode bits to restore
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

// `pwrite` equivalent. Same platform notes as above, [`read_at`].
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

// Ensures handling of errors that would otherwise only be caught
// when the `File` is closed,
pub fn sync(f: &File) -> Result<(), FsError> {
    f.sync_all().map_err(FsError::Other)
}

// ---------- path helpers for ingest ----------

// Case-insensitive duplicate detector. Returns the first colliding pair.
// Windows/macOS filesystems would silently merge these on fetch.
// Ingest must hard-error instead.
pub fn find_case_collision(paths: &[String]) -> Option<(String, String)> {
    let mut seen: std::collections::HashMap<String, &String> = std::collections::HashMap::new();
    for p in paths {
        let folded = p.to_lowercase();
        if let Some(prev) = seen.insert(folded, p) {
            return Some((prev.clone(), p.clone()));
        }
    }
    None
}

// Relative path -> `/`-separated UTF-8 form for the manifest.
// `None` means unrepresentable (non-UTF8, absolute, `..`, empty) and
// call-site skips it under its leniency policy.
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
