use std::{
    fmt, io,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
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
