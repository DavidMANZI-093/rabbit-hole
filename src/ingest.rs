use std::{
    collections::HashMap,
    fmt,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{
    debug,
    fs::{self, FileMeta, FsError, Kind, Op},
    protocol::manifest::{
        FileEntry, Manifest,
        common::{MAX_BLOCK_SIZE, MIN_BLOCK_SIZE, validate_path},
    },
    utils::progress::{IngestProgress, Phase, SkipReason},
};

const WIDTH: usize = 128 * 1024; // 131,072 bytes or ~128KB

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

#[derive(Default)]
pub struct IngestStats {
    pub files: usize,
    pub bytes: u64,
    pub blocks_total: u64,
    pub blocks_unique: usize,
    pub skipped: usize,
}

pub fn ingest(
    src: &Path,
    block_size: u32,
    progress: &IngestProgress,
) -> Result<(Manifest, IngestStats), IngestError> {
    if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size) {
        return Err(IngestError::Walk(format!(
            "block size must be {MIN_BLOCK_SIZE}..={MAX_BLOCK_SIZE}"
        )));
    }

    let mut pool: Vec<[u8; 32]> = Vec::new();
    let mut index: HashMap<[u8; 32], u32> = HashMap::new();
    let mut files: Vec<FileEntry> = Vec::new();
    let mut stats = IngestStats::default();

    let meta = fs::metadata(src).map_err(IngestError::Fs)?;

    match meta.kind {
        Kind::Symlink => Err(IngestError::BadName(format!(
            "refusing symlink {} (share target directly)",
            src.display()
        )))?,
        Kind::File => {
            let name = single_file_manifest_path(src)?;
            progress.set_totals(1, meta.size);
            progress.set_stage(Phase::Hash, &name);

            let entry = hash_file(
                src, &name, &meta, block_size, &mut pool, &mut index, &mut stats, progress,
            )?;
            files.push(entry);
        }
        Kind::Dir => {
            let mut paths: Vec<(PathBuf, u64, String)> = Vec::new();
            let walker = walkdir::WalkDir::new(src)
                .follow_links(false)
                .min_depth(1)
                .into_iter();

            for (i, dent) in walker.enumerate() {
                let dent = match dent {
                    Ok(d) => d,
                    Err(e) => {
                        debug!(
                            "subsequent walk: failed to walk directory with error: {}",
                            e.to_string()
                        );
                        stats.skipped += 1;
                        progress.inc_skipped(SkipReason::WalkError);
                        continue;
                    }
                };

                // Stage updates are throttled inside, but `set_stage` still
                // formats; batch here so huge trees don't pay per-dent.
                if i % 32 == 0 {
                    progress.set_stage(Phase::Scan, &dent.path().display().to_string());
                }

                if dent.file_type().is_symlink() {
                    debug!(
                        "subsequent walk: refusing symlink {}",
                        dent.path().display()
                    );
                    stats.skipped += 1;
                    progress.inc_skipped(SkipReason::Symlink);
                    continue;
                }

                if !dent.file_type().is_file() {
                    // Subdirectories are expected walk entries, not skips.
                    if dent.file_type().is_dir() {
                        continue;
                    }
                    debug!(
                        "subsequent walk: not a regular file {}",
                        dent.path().display()
                    );
                    stats.skipped += 1;
                    progress.inc_skipped(SkipReason::NonFile);
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
                        stats.skipped += 1;
                        progress.inc_skipped(SkipReason::StatFailed);
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
                        stats.skipped += 1;
                        progress.inc_skipped(SkipReason::BadName);
                        continue;
                    }
                };

                paths.push((abs, size, rel));
            }
            paths.sort_by(|a, b| a.2.cmp(&b.2));

            let total_bytes: u64 = paths.iter().map(|(_, s, _)| *s).sum();
            progress.set_totals(paths.len(), total_bytes);
            progress.set_stage(Phase::Check, "sorting / collision check");

            let rels: Vec<String> = paths.iter().map(|(_, _, r)| r.clone()).collect();
            if let Some((a, b)) = fs::find_case_collision(&rels) {
                return Err(IngestError::Collision(a, b));
            }

            for (abs, _, rel) in &paths {
                let fmeta = crate::fs::metadata(abs).map_err(IngestError::Fs)?;
                progress.set_stage(Phase::Hash, rel);
                let entry = hash_file(
                    abs, rel, &fmeta, block_size, &mut pool, &mut index, &mut stats, progress,
                )?;
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

    stats.blocks_unique = pool.len();
    let manifest = Manifest {
        block_size,
        pool,
        files,
    };
    manifest
        .validate()
        .map_err(|e| IngestError::Walk(format!("post-ingest error {}", e.to_string())));
    Ok((manifest, stats))
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

fn hash_file(
    abs: &Path,
    rel: &str,
    meta: &FileMeta,
    block_size: u32,
    pool: &mut Vec<[u8; 32]>,
    index: &mut HashMap<[u8; 32], u32>,
    stats: &mut IngestStats,
    progress: &IngestProgress,
) -> Result<FileEntry, IngestError> {
    let snap_pool = pool.len();
    let snap_blocks = stats.blocks_total;
    let snap_bytes = stats.bytes;

    match hash_file_inner(abs, rel, meta, block_size, pool, index, stats, progress) {
        Ok(e) => Ok(e),
        Err(err) => {
            pool.truncate(snap_pool);
            index.retain(|_, id| (*id as usize) < snap_pool);
            stats.blocks_total = snap_blocks;
            stats.bytes = snap_bytes;
            Err(err)
        }
    }
}

fn hash_file_inner(
    abs: &Path,
    rel: &str,
    meta: &FileMeta,
    block_size: u32,
    pool: &mut Vec<[u8; 32]>,
    index: &mut HashMap<[u8; 32], u32>,
    stats: &mut IngestStats,
    progress: &IngestProgress,
) -> Result<FileEntry, IngestError> {
    let bs = block_size as u64;
    let mut chunks: Vec<u32> = Vec::new();
    if meta.size > 0 {
        chunks.reserve(meta.size.div_ceil(bs).min(MAX_BLOCK_SIZE.into()) as usize);

        let mut f =
            File::open(abs).map_err(|e| IngestError::Fs(fs::map_io(abs, fs::Op::Read, e)))?;
        let mut buffer = vec![0u8; WIDTH];
        let mut hasher = blake3::Hasher::new();
        let mut in_block: u64 = 0;

        loop {
            let n = f
                .read(&mut buffer)
                .map_err(|e| IngestError::Fs(fs::map_io(abs, Op::Read, e)))?;
            if n == 0 {
                break;
            }
            stats.bytes += n as u64;
            progress.add_bytes(n as u64);

            let mut offset = 0;
            while offset < n {
                let want = (bs - in_block).min((n - offset) as u64) as usize;
                hasher.update(&buffer[offset..offset + want]);
                offset += want;
                in_block += want as u64;

                if in_block == bs {
                    let unique = push_block(&hasher.finalize(), pool, index, &mut chunks, stats);
                    progress.inc_blocks(unique);
                    hasher = blake3::Hasher::new();
                    in_block = 0;
                }
            }
        }

        if in_block > 0 {
            let unique = push_block(&hasher.finalize(), pool, index, &mut chunks, stats);
            progress.inc_blocks(unique);
        }
    }
    stats.files += 1;
    progress.inc_files(1);

    Ok(FileEntry {
        path: rel.to_string(),
        size: meta.size,
        mode: meta.mode,
        mtime_ns: meta.mtime_ns,
        chunks,
    })
}

fn push_block(
    digest: &blake3::Hash,
    pool: &mut Vec<[u8; 32]>,
    index: &mut HashMap<[u8; 32], u32>,
    chunks: &mut Vec<u32>,
    stats: &mut IngestStats,
) -> bool {
    let bytes = *digest.as_bytes();
    let (id, unique) = match index.get(&bytes) {
        Some(&id) => (id, false),
        None => {
            let id = pool.len() as u32;
            pool.push(bytes);
            index.insert(bytes, id);
            (id, true)
        }
    };

    chunks.push(id);
    stats.blocks_total += 1;
    unique
}
