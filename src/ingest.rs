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
        common::{MAX_BLOCK_SIZE, MAX_BLOCKS, MIN_BLOCK_SIZE, validate_path},
    },
    utils::progress::{IngestProgress, Phase, SkipReason},
};

const READ_BUF_SIZE: usize = 128 * 1024;

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
                        debug!("walk error: {e}");
                        stats.skipped += 1;
                        progress.inc_skipped(SkipReason::WalkError);
                        continue;
                    }
                };

                // set_stage throttles internally but still allocates; batch every 32 to avoid the cost
                if i % 32 == 0 {
                    progress.set_stage(Phase::Scan, &dent.path().display().to_string());
                }

                if dent.file_type().is_symlink() {
                    debug!("skipping symlink {}", dent.path().display());
                    stats.skipped += 1;
                    progress.inc_skipped(SkipReason::Symlink);
                    continue;
                }

                if !dent.file_type().is_file() {
                    // Subdirectories are expected walk entries, not skips.
                    if dent.file_type().is_dir() {
                        continue;
                    }
                    debug!("skipping non-file {}", dent.path().display());
                    stats.skipped += 1;
                    progress.inc_skipped(SkipReason::NonFile);
                    continue;
                }

                let abs = dent.path().to_path_buf();
                let size = match fs::metadata(&abs) {
                    Ok(m) => m.size,
                    Err(e) => {
                        debug!("stat failed {}: {e}", dent.path().display());
                        stats.skipped += 1;
                        progress.inc_skipped(SkipReason::StatFailed);
                        continue;
                    }
                };
                let rel = abs.strip_prefix(src).map_err(|_| {
                    IngestError::BadName(format!("path outside source root: {}", abs.display()))
                })?;
                let rel = match to_manifest_path(rel, &abs) {
                    Ok(r) => r,
                    Err(e) => {
                        debug!("bad manifest path {}: {e}", abs.display());
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

            let rels: Vec<&str> = paths.iter().map(|(_, _, r)| r.as_str()).collect();
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
        .map_err(|e| IngestError::Walk(format!("post-ingest error {e}")))?;
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
    let saved_pool_len = pool.len();
    let saved_blocks_total = stats.blocks_total;
    let saved_bytes = stats.bytes;

    match hash_file_inner(abs, rel, meta, block_size, pool, index, stats, progress) {
        Ok(e) => Ok(e),
        Err(err) => {
            pool.truncate(saved_pool_len);
            index.retain(|_, id| (*id as usize) < saved_pool_len);
            stats.blocks_total = saved_blocks_total;
            stats.bytes = saved_bytes;
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
        chunks.reserve(meta.size.div_ceil(bs).min(MAX_BLOCKS as u64) as usize);

        let mut f =
            File::open(abs).map_err(|e| IngestError::Fs(fs::map_io(abs, fs::Op::Read, e)))?;
        let mut buffer = vec![0u8; READ_BUF_SIZE];
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // --- push_block ---

    fn make_hash(byte: u8) -> blake3::Hash {
        blake3::hash(&[byte])
    }

    #[test]
    fn first_block_is_unique() {
        let mut pool = Vec::new();
        let mut index = HashMap::new();
        let mut chunks = Vec::new();
        let mut stats = IngestStats::default();
        let unique = push_block(&make_hash(1), &mut pool, &mut index, &mut chunks, &mut stats);
        assert!(unique);
        assert_eq!(pool.len(), 1);
        assert_eq!(chunks, [0u32]);
        assert_eq!(stats.blocks_total, 1);
    }

    #[test]
    fn duplicate_block_is_not_unique() {
        let mut pool = Vec::new();
        let mut index = HashMap::new();
        let mut chunks = Vec::new();
        let mut stats = IngestStats::default();
        let h = make_hash(42);
        push_block(&h, &mut pool, &mut index, &mut chunks, &mut stats);
        let unique = push_block(&h, &mut pool, &mut index, &mut chunks, &mut stats);
        assert!(!unique);
        assert_eq!(pool.len(), 1, "pool must not grow on duplicate");
        assert_eq!(chunks, [0u32, 0u32], "both chunks reference same pool slot");
        assert_eq!(stats.blocks_total, 2);
    }

    #[test]
    fn distinct_blocks_fill_pool_sequentially() {
        let mut pool = Vec::new();
        let mut index = HashMap::new();
        let mut chunks = Vec::new();
        let mut stats = IngestStats::default();
        for i in 0..4u8 {
            push_block(&make_hash(i), &mut pool, &mut index, &mut chunks, &mut stats);
        }
        assert_eq!(pool.len(), 4);
        assert_eq!(chunks, [0, 1, 2, 3]);
    }

    // --- to_manifest_path ---

    #[test]
    fn simple_filename_passes_through() {
        let rel = Path::new("file.txt");
        let abs = Path::new("/tmp/file.txt");
        assert_eq!(to_manifest_path(rel, abs).unwrap(), "file.txt");
    }

    #[test]
    fn nested_path_uses_forward_slashes() {
        let rel = Path::new("a/b/c.bin");
        let abs = Path::new("/data/a/b/c.bin");
        let result = to_manifest_path(rel, abs).unwrap();
        assert!(!result.contains('\\'), "must use forward slashes");
        assert_eq!(result, "a/b/c.bin");
    }

    #[test]
    fn dotdot_component_is_rejected_by_validate_path() {
        // validate_path inside to_manifest_path must catch traversal
        let rel = Path::new("../escape.txt");
        let abs = Path::new("/tmp/../escape.txt");
        assert!(to_manifest_path(rel, abs).is_err());
    }

    // --- ingest integration round-trip ---

    #[test]
    fn single_file_ingest_produces_correct_stats() {
        let dir = std::env::temp_dir().join(format!("rh-ingest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hello.txt");
        std::fs::write(&file, b"hello, rabbit-hole").unwrap();

        let progress = crate::utils::progress::IngestProgress::hidden();
        let (manifest, stats) = ingest(&file, crate::protocol::manifest::common::DEFAULT_BLOCK_SIZE, &progress)
            .expect("ingest must succeed");

        assert_eq!(stats.files, 1);
        assert_eq!(stats.bytes, 18);
        assert_eq!(stats.blocks_total, 1);
        assert_eq!(stats.blocks_unique, 1);
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "hello.txt");
        assert_eq!(manifest.files[0].size, 18);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_file_content_deduplicates_blocks() {
        let dir = std::env::temp_dir().join(format!("rh-dedup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Two files with identical content — should produce 1 unique block, 2 total
        let content = vec![0xABu8; 1024];
        std::fs::write(dir.join("a.bin"), &content).unwrap();
        std::fs::write(dir.join("b.bin"), &content).unwrap();

        let progress = crate::utils::progress::IngestProgress::hidden();
        let (manifest, stats) = ingest(&dir, crate::protocol::manifest::common::DEFAULT_BLOCK_SIZE, &progress)
            .expect("ingest must succeed");

        assert_eq!(stats.files, 2);
        assert_eq!(stats.blocks_total, 2, "two files = two block references");
        assert_eq!(stats.blocks_unique, 1, "identical content = one unique block");
        assert_eq!(manifest.pool.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
