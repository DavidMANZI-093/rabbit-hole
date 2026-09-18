use std::{
    fmt,
    path::{Path, PathBuf},
};

use crate::{
    debug,
    fs::{self, FsError, Kind},
    protocol::manifest::{
        FileEntry, Manifest,
        common::{MAX_BLOCK_SIZE, MIN_BLOCK_SIZE, validate_path},
    },
};

#[derive(Debug)]
pub enum IngestError {
    Fs(FsError),
    Walk(String),
    Collision(String, String),
    BadName(String),
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fs(e) => write!(f, "{e}"),
            Self::Walk(e) => write!(f, "walk: {e}"),
            Self::Collision(a, b) => write!(
                f,
                "case collision: {a:?} and {b:?} would merge on case-insensitive filesystems"
            ),
            Self::BadName(n) => write!(f, "{n}"),
        }
    }
}

pub fn ingest(src: &Path, block_size: u32) -> Result<Manifest, IngestError> {
    if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size) {
        return Err(IngestError::Walk(format!(
            "block size must be {MIN_BLOCK_SIZE}..={MAX_BLOCK_SIZE}"
        )));
    }

    let mut pool: Vec<[u8; 32]> = Vec::new();
    let mut files: Vec<FileEntry> = Vec::new();

    let meta = fs::metadata(src).map_err(IngestError::Fs)?;

    match meta.kind {
        Kind::Symlink => Err(IngestError::BadName(format!(
            "refusing symlink {} (share target directly)",
            src.display()
        )))?,
        Kind::File => {
            let name = single_file_manifest_path(src)?;

            // TODO: continue with hashing...
            let entry = FileEntry {
                path: String::from(""),
                size: 0,
                mode: None,
                mtime_ns: None,
                chunks: [].to_vec(),
            };

            files.push(entry);
        }
        Kind::Dir => {
            let mut paths: Vec<(PathBuf, u64, String)> = Vec::new();
            let walker = walkdir::WalkDir::new(src)
                .follow_links(false)
                .min_depth(1)
                .into_iter();

            for dent in walker {
                let dent = match dent {
                    Ok(d) => d,
                    Err(e) => {
                        debug!(
                            "subsequent walk: failed to walk directory with error: {}",
                            e.to_string()
                        );
                        continue;
                    }
                };

                if dent.file_type().is_symlink() {
                    debug!(
                        "subsequent walk: refusing symlink {}",
                        dent.path().display()
                    );
                    continue;
                }

                if !dent.file_type().is_file() {
                    debug!(
                        "subsequent walk: not a regular file {}",
                        dent.path().display()
                    );
                    continue;
                }

                let abs = dent.path().to_path_buf();
                let size = match fs::metadata(&abs) {
                    Ok(m) => m.size,
                    Err(e) => {
                        debug!(
                            "subsequent walk: broken file, invalid size {} with error {}",
                            dent.path().display(),
                            e.to_string()
                        );
                        continue;
                    }
                };
                let rel = abs.strip_prefix(src).map_err(|_| {
                    IngestError::BadName(format!("path escapess source root: {}", abs.display()))
                })?;
                let rel = match to_manifest_path(rel, &abs) {
                    Ok(r) => r,
                    Err(e) => {
                        debug!(
                            "failed to validate for manifest with error {}",
                            e.to_string()
                        );
                        continue;
                    }
                };

                paths.push((abs, size, rel));
            }
            paths.sort_by(|a, b| a.2.cmp(&b.2));

            let rels: Vec<String> = paths.iter().map(|(_, _, r)| r.clone()).collect();
            if let Some((a, b)) = fs::find_case_collision(&rels) {
                return Err(IngestError::Collision(a, b));
            }

            for (abs, _, rel) in paths {
                // TODO: continue with hashing...
                let entry = FileEntry {
                    path: String::from(""),
                    size: 0,
                    mode: None,
                    mtime_ns: None,
                    chunks: [].to_vec(),
                };

                files.push(entry);
            }
        }
        Kind::Other => {
            return Err(IngestError::Walk(format!(
                "unsupported file type {}",
                src.display()
            )));
        }
    }

    Ok(Manifest {
        block_size: 1024 * 1024,
        pool: [].to_vec(),
        files: [].to_vec(),
    })
}

fn single_file_manifest_path(src: &Path) -> Result<String, IngestError> {
    let file_os = src
        .file_name()
        .ok_or_else(|| IngestError::BadName(format!("non-UTF-8 file name {}", src.display())))?;
    to_manifest_path(Path::new(file_os), src)
}

fn to_manifest_path(rel: &Path, abs: &Path) -> Result<String, IngestError> {
    let unix = fs::rel_to_unix(rel).ok_or_else(|| {
        IngestError::BadName(format!(
            "cannot represent as relative UTF-8 path: {}",
            abs.display()
        ))
    })?;
    validate_path(&unix).map_err(|e| {
        IngestError::BadName(format!("invalid path {unix:?} ({e}): {}", abs.display()))
    })?;
    Ok(unix)
}
