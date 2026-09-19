use crate::protocol::manifest::common::{
    MAX_BLOCK_SIZE, MAX_BLOCKS, MAX_FILES, MAX_PATH_LEN, MIN_BLOCK_SIZE, ManifestError,
    expected_chunks, validate_path,
};

pub mod common;
pub mod v1;

// Encode uses CURRENT; decode accepts CURRENT and predecessors
// per the support policy (current + previous).
pub const CURRENT: u16 = 1;

#[derive(Clone)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mode: Option<u32>,
    pub mtime_ns: Option<i64>,
    pub chunks: Vec<u32>,
}

#[derive(Clone)]
pub struct Manifest {
    pub block_size: u32,
    pub pool: Vec<[u8; 32]>,
    pub files: Vec<FileEntry>,
}

impl Manifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&self.block_size) {
            return Err(ManifestError::BadBlockSize(self.block_size));
        }
        if self.files.len() as u64 > MAX_FILES as u64 {
            return Err(ManifestError::OverLimit(
                self.files.len() as u64,
                MAX_FILES as u64,
            ));
        }
        if self.pool.len() as u64 > MAX_BLOCKS as u64 {
            return Err(ManifestError::OverLimit(
                self.pool.len() as u64,
                MAX_BLOCKS as u64,
            ));
        }
        let mut seen = std::collections::HashSet::with_capacity(self.files.len());
        for f in &self.files {
            if f.path.len() > MAX_PATH_LEN {
                return Err(ManifestError::OverLimit(
                    f.path.len() as u64,
                    MAX_PATH_LEN as u64,
                ));
            }
            validate_path(&f.path)?;
            if !seen.insert(&f.path) {
                return Err(ManifestError::DuplicatePath(f.path.clone()));
            }
            let want = expected_chunks(f.size, self.block_size);
            if f.chunks.len() as u64 != want {
                return Err(ManifestError::ChunkMismatch {
                    got: f.chunks.len() as u64,
                    size: f.size,
                    bs: self.block_size,
                });
            }
            if f.chunks.len() as u64 > MAX_BLOCKS as u64 {
                return Err(ManifestError::OverLimit(
                    f.chunks.len() as u64,
                    MAX_BLOCKS as u64,
                ));
            }
            for &c in &f.chunks {
                if (c as usize) >= self.pool.len() {
                    return Err(ManifestError::BlockIndexOob(c as u64, self.pool.len()));
                }
            }
        }
        Ok(())
    }
}

// ---------- version dispatch ----------

pub fn encode(m: &Manifest) -> Vec<u8> {
    match CURRENT {
        1 => v1::encode(m),
        _ => unreachable!("CURRENT has no encode"),
    }
}
