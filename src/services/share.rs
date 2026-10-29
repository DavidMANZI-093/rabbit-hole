use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use rand::Rng;

use crate::protocol::{self, manifest::Manifest};

#[derive(Clone)]
pub struct Locator {
    pub abs: PathBuf,
    pub offset: u64,
    pub len: u32,
}

pub struct DirFile {
    pub rel: String,
    pub size: u64,
}

pub enum Kind {
    Single {
        abs: PathBuf,
        name: String,
        size: u64,
    },
    Dir {
        root: PathBuf,
        files: Vec<DirFile>,
    },
}

pub struct Shared {
    pub manifest_bytes: Vec<u8>,
    pub etag: String,
    pub kind: Kind,
    pub blocks: HashMap<[u8; 32], Arc<Vec<Locator>>>,
    pub token: Option<String>,
}

#[derive(Debug)]
pub enum BlockError {
    Unknown,
    Failed(String),
}

pub fn build_shared(
    manifest: &Manifest,
    src: &Path,
    token: Option<String>,
) -> Result<Arc<Shared>, String> {
    // Validate before encoding: 'encode()' debug-asserts validity, which
    // panics in debug builds instead of returning Err on corrupt input.
    manifest
        .validate()
        .map_err(|e| format!("invalid manifest: {e}"))?;

    let manifest_bytes = protocol::manifest::encode(manifest);
    let etag = blake3::hash(&manifest_bytes).to_hex().to_string();
    let bs = manifest.block_size as u64;
    let single = src.is_file();

    let mut blocks_build: HashMap<[u8; 32], Vec<Locator>> = HashMap::new();
    let root_prefix: Option<String> = manifest.root.clone().map(|r| format!("{r}/"));
    for f in &manifest.files {
        let rel = match &root_prefix {
            Some(prefix) => f.path.strip_prefix(prefix.as_str()).ok_or_else(|| {
                format!(
                    "manifest path {} outside root {}",
                    f.path,
                    manifest.root.as_deref().unwrap_or("?")
                )
            })?,
            None => f.path.as_str(),
        };
        let abs = if single {
            src.to_path_buf()
        } else {
            src.join(rel)
        };

        for (j, idx) in f.chunks.iter().enumerate() {
            let digest = manifest.pool[*idx as usize];
            let offset = j as u64 * bs;
            let len = bs.min(f.size.saturating_sub(offset)) as u32;

            blocks_build.entry(digest).or_default().push(Locator {
                abs: abs.clone(),
                offset,
                len,
            });
        }
    }
    let blocks: HashMap<[u8; 32], Arc<Vec<Locator>>> = blocks_build
        .into_iter()
        .map(|(k, v)| (k, Arc::new(v)))
        .collect();

    let kind = if single {
        let entry = manifest
            .files
            .first()
            .ok_or("single-file manifest must contain exactly one file")?;
        if manifest.files.len() != 1 {
            return Err("single-file share expects exactly one manifest entry".into());
        }

        let live =
            crate::utils::fs::metadata(src).map_err(|e| format!("stat {}: {e}", src.display()))?;
        if live.kind != crate::utils::fs::Kind::File {
            return Err(format!("refusing non-file {}", src.display()));
        }
        if live.size != entry.size {
            return Err(format!(
                "source changed since ingest (manifest {} B, disk {} B): re-share",
                entry.size, live.size
            ));
        }

        let name = src
            .file_name()
            .and_then(std::ffi::os_str::OsStr::to_str)
            .unwrap_or("download")
            .to_string();
        Kind::Single {
            abs: src.to_path_buf(),
            name,
            size: entry.size,
        }
    } else {
        Kind::Dir {
            root: src.to_path_buf(),
            files: manifest
                .files
                .iter()
                .map(|f| DirFile {
                    rel: f.path.clone(),
                    size: f.size,
                })
                .collect(),
        }
    };

    Ok(Arc::new(Shared {
        manifest_bytes,
        etag,
        kind,
        blocks,
        token,
    }))
}

impl Shared {
    // Reads the first locator whose bytes verify against `want`.
    pub fn read_block(&self, want: &[u8; 32]) -> Result<Vec<u8>, BlockError> {
        let Some(cands) = self.blocks.get(want) else {
            return Err(BlockError::Unknown);
        };
        read_verified_block(want, cands.as_slice()).map_err(BlockError::Failed)
    }
}

fn read_verified_block(want: &[u8; 32], cands: &[Locator]) -> Result<Vec<u8>, String> {
    for loc in cands {
        let f = match std::fs::File::open(&loc.abs) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut buf = vec![0u8; loc.len as usize];
        let mut off = 0;
        while off < buf.len() {
            match crate::utils::fs::read_at(&f, &mut buf[off..], loc.offset + off as u64) {
                Ok(0) => break,
                Ok(n) => off += n,
                Err(_) => break,
            }
        }
        if off != buf.len() {
            continue;
        }
        if blake3::hash(&buf).as_bytes() == want {
            return Ok(buf);
        }
    }
    Err("block failed verification (source changed on disk?)".into())
}

pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    crate::protocol::manifest::common::hex_encode(&buf)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const BLOCK: &[u8] = b"hello block";

    fn temp_file(tag: &str, bytes: &[u8]) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("rh-block-{}-{tag}-{n}", std::process::id()));
        std::fs::write(&path, bytes).expect("write temp block");
        path
    }

    fn locator(path: &Path, len: u32) -> Locator {
        Locator {
            abs: path.to_path_buf(),
            offset: 0,
            len,
        }
    }

    fn digest(bytes: &[u8]) -> [u8; 32] {
        *blake3::hash(bytes).as_bytes()
    }

    #[test]
    fn read_block_returns_verified_bytes() {
        let path = temp_file("ok", BLOCK);
        let locs = [locator(&path, BLOCK.len() as u32)];
        let got = read_verified_block(&digest(BLOCK), &locs).expect("verifies");
        assert_eq!(got, BLOCK);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_block_rejects_wrong_hash() {
        let path = temp_file("wrong", BLOCK);
        let locs = [locator(&path, BLOCK.len() as u32)];
        assert!(read_verified_block(&digest(b"different"), &locs).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_block_rejects_short_read() {
        let path = temp_file("short", BLOCK);
        let locs = [locator(&path, BLOCK.len() as u32 + 10)];
        assert!(read_verified_block(&digest(BLOCK), &locs).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_block_falls_back_to_a_good_candidate() {
        let missing = std::env::temp_dir().join("rh-block-does-not-exist");
        let path = temp_file("fallback", BLOCK);
        let locs = [
            locator(&missing, BLOCK.len() as u32),
            locator(&path, BLOCK.len() as u32),
        ];
        let got = read_verified_block(&digest(BLOCK), &locs).expect("second candidate verifies");
        assert_eq!(got, BLOCK);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_block_reports_unknown_digest() {
        let shared = Shared {
            manifest_bytes: Vec::new(),
            etag: String::new(),
            kind: Kind::Dir {
                root: PathBuf::new(),
                files: Vec::new(),
            },
            blocks: HashMap::new(),
            token: None,
        };
        assert!(matches!(
            shared.read_block(&[0u8; 32]),
            Err(BlockError::Unknown)
        ));
    }

    fn rooted_manifest() -> crate::protocol::manifest::Manifest {
        use crate::protocol::manifest::common::DEFAULT_BLOCK_SIZE;
        use crate::protocol::manifest::{FileEntry, Manifest};
        Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: Some("photos".to_string()),
            pool: vec![[0u8; 32]],
            files: vec![FileEntry {
                path: "photos/a.txt".to_string(),
                size: 1,
                mode: None,
                mtime_ns: None,
                chunks: vec![0],
            }],
        }
    }

    #[test]
    fn build_shared_strips_recorded_root() {
        let manifest = rooted_manifest();
        let shared = build_shared(&manifest, std::path::Path::new("/srv/share/photos"), None)
            .expect("build must succeed");
        let locs = &shared.blocks[&[0u8; 32]];
        assert_eq!(locs.len(), 1);
        // Without stripping, this would be .../photos/photos/a.txt.
        assert_eq!(
            locs[0].abs,
            std::path::PathBuf::from("/srv/share/photos/a.txt")
        );
    }

    #[test]
    fn build_shared_rejects_path_outside_root() {
        let mut manifest = rooted_manifest();
        manifest.files[0].path = "other/a.txt".to_string();
        assert!(build_shared(&manifest, std::path::Path::new("/srv/share/photos"), None).is_err());
    }
}
