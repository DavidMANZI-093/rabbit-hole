use crate::protocol::manifest::common::{
    MAX_BLOCK_SIZE, MAX_BLOCKS, MAX_FILES, MAX_MANIFEST_BYTES, MAX_PATH_LEN, MIN_BLOCK_SIZE,
    ManifestError, expected_chunks, validate_path,
};

pub mod common;
pub mod v1;
pub mod v2;

// Decode accepts CURRENT and predecessors per the support policy
// (current + previous). Encode uses the lowest version expressing the
// content (see 'encode').
pub const CURRENT: u16 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mode: Option<u32>,
    pub mtime_ns: Option<i64>,
    pub chunks: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub block_size: u32,
    pub pool: Vec<[u8; 32]>,
    pub files: Vec<FileEntry>,
    pub root: Option<String>,
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
        if let Some(r) = &self.root {
            validate_path(r)?;
            if r.contains('/') {
                return Err(ManifestError::BadPath(
                    r.clone(),
                    "root must be a single path component",
                ));
            }
            let prefix = format!("{r}/");
            for f in &self.files {
                if !f.path.starts_with(prefix.as_str()) {
                    return Err(ManifestError::BadPath(
                        f.path.clone(),
                        "outside manifest root",
                    ));
                }
            }
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
    match &m.root {
        Some(_) => v2::encode(m),
        None => v1::encode(m),
        // NOTE: broken/unknown arm is handled by 'peek_version()' at decode stage.
        // This allows for manifest shape-based extension.
    }
}

pub fn decode(input: &[u8]) -> Result<Manifest, ManifestError> {
    if input.len() > MAX_MANIFEST_BYTES as usize {
        return Err(ManifestError::TooLarge(MAX_MANIFEST_BYTES));
    }
    match peek_version(input)? {
        1 => v1::decode(input),
        2 => v2::decode(input),
        v => Err(ManifestError::UnsupportedVersion(v)),
    }
}

pub fn peek_version(input: &[u8]) -> Result<u16, common::ManifestError> {
    if input.len() < 4 {
        return Err(common::ManifestError::Truncated { at: 0, need: 4 });
    }
    if input[..4] != v1::MAGIC {
        return Err(common::ManifestError::BadMagic);
    }
    if input.len() < 6 {
        return Err(common::ManifestError::Truncated { at: 4, need: 2 });
    }
    Ok(u16::from_le_bytes([input[4], input[5]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::manifest::common::DEFAULT_BLOCK_SIZE;

    fn make_hash(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn empty_manifest_roundtrips() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![],
            files: vec![],
        };
        let encoded = encode(&m);
        let decoded = decode(&encoded).expect("decode failed");
        assert_eq!(m, decoded);
    }

    #[test]
    fn single_file_no_metadata_roundtrips() {
        let hash = make_hash(1);
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![hash],
            files: vec![FileEntry {
                path: "hello.txt".to_string(),
                size: DEFAULT_BLOCK_SIZE as u64,
                mode: None,
                mtime_ns: None,
                chunks: vec![0],
            }],
        };
        let encoded = encode(&m);
        let decoded = decode(&encoded).expect("decode failed");
        assert_eq!(m, decoded);
    }

    #[test]
    fn single_file_with_mode_and_mtime_roundtrips() {
        let hash = make_hash(2);
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![hash],
            files: vec![FileEntry {
                path: "script.sh".to_string(),
                size: DEFAULT_BLOCK_SIZE as u64,
                mode: Some(0o755),
                mtime_ns: Some(1_700_000_000_000_000_000),
                chunks: vec![0],
            }],
        };
        let encoded = encode(&m);
        let decoded = decode(&encoded).expect("decode failed");
        assert_eq!(m, decoded);
    }

    #[test]
    fn multifile_with_shared_block_roundtrips() {
        let hash = make_hash(3);
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![hash],
            files: vec![
                FileEntry {
                    path: "a/foo.bin".to_string(),
                    size: DEFAULT_BLOCK_SIZE as u64,
                    mode: None,
                    mtime_ns: None,
                    chunks: vec![0],
                },
                FileEntry {
                    path: "b/foo.bin".to_string(),
                    size: DEFAULT_BLOCK_SIZE as u64,
                    mode: None,
                    mtime_ns: None,
                    chunks: vec![0], // same block — dedup
                },
            ],
        };
        let encoded = encode(&m);
        let decoded = decode(&encoded).expect("decode failed");
        assert_eq!(m, decoded);
    }

    #[test]
    fn negative_mtime_roundtrips() {
        let hash = make_hash(4);
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![hash],
            files: vec![FileEntry {
                path: "old.txt".to_string(),
                size: DEFAULT_BLOCK_SIZE as u64,
                mode: None,
                mtime_ns: Some(-1_000_000_000),
                chunks: vec![0],
            }],
        };
        let encoded = encode(&m);
        let decoded = decode(&encoded).expect("decode failed");
        assert_eq!(m, decoded);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bad = vec![0u8; 16];
        bad[0] = b'X';
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn truncated_input_is_rejected() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![],
            files: vec![],
        };
        let full = encode(&m);
        assert!(decode(&full[..full.len() - 1]).is_err());
    }

    #[test]
    fn encode_picks_v1_for_rootless() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![],
            files: vec![],
        };
        assert_eq!(peek_version(&encode(&m)).unwrap(), 1);
    }

    #[test]
    fn encode_picks_v2_for_rooted() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: Some("photos".to_string()),
            pool: vec![],
            files: vec![],
        };
        let bytes = encode(&m);
        assert_eq!(peek_version(&bytes).unwrap(), 2);
        assert_eq!(decode(&bytes).expect("decode failed"), m);
    }

    #[test]
    fn frozen_v1_bytes_decode_flat() {
        let bytes = vec![
            b'R', b'H', b'B', b'1', // magic
            1, 0, // version
            0, 0, // flags
            0, 0, 16, 0, // block size 1 MiB LE
            0, // file count
            0, // pool count
        ];
        let m = decode(&bytes).expect("decode failed");
        assert_eq!(m.root, None);
        assert!(m.files.is_empty());
    }

    #[test]
    fn future_version_rejected_with_upgrade_error() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![],
            files: vec![],
        };
        let mut bytes = encode(&m);
        bytes[4] = 9;
        bytes[5] = 0;
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, ManifestError::UnsupportedVersion(9)));
    }

    #[test]
    fn rooted_manifest_validates() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: Some("photos".to_string()),
            pool: vec![],
            files: vec![FileEntry {
                path: "photos/a.txt".to_string(),
                size: 0,
                mode: None,
                mtime_ns: None,
                chunks: vec![],
            }],
        };
        assert!(m.validate().is_ok());
    }

    #[test]
    fn root_with_slash_rejected() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: Some("a/b".to_string()),
            pool: vec![],
            files: vec![],
        };
        assert!(m.validate().is_err());
    }

    #[test]
    fn file_outside_root_rejected() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: Some("photos".to_string()),
            pool: vec![],
            files: vec![FileEntry {
                path: "other/a.txt".to_string(),
                size: 0,
                mode: None,
                mtime_ns: None,
                chunks: vec![],
            }],
        };
        assert!(m.validate().is_err());
    }
}
