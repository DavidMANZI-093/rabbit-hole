use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use rand::Rng;

use crate::protocol::{self, manifest::Manifest};

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
    pub blocks: HashMap<[u8; 32], Vec<Locator>>,
    pub token: Option<String>,
}

// 32 random bytes as lowercase hex. CSPRNG via `rand`; fixed width keeps
// parsing trivial and comparison safely constant-time.
pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);

    let mut s = String::with_capacity(64);
    for b in buf {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 15) as u32, 16).unwrap());
    }

    s
}

pub fn build_shared(
    manifest: &Manifest,
    src: &Path,
    token: Option<String>,
) -> Result<Shared, String> {
    let manifest_bytes = protocol::manifest::encode(manifest);
    let etag = blake3::hash(&manifest_bytes).to_hex().to_string();
    let bs = manifest.block_size as u64;
    let single = src.is_file();

    let mut blocks: HashMap<[u8; 32], Vec<Locator>> = HashMap::new();
    for f in &manifest.files {
        let abs = if single {
            src.to_path_buf()
        } else {
            src.join(&f.path)
        };

        for (j, idx) in f.chunks.iter().enumerate() {
            let digest = manifest.pool[*idx as usize];
            let offset = j as u64 * bs;
            let len = bs.min(f.size.saturating_sub(offset)) as u32;

            blocks.entry(digest).or_default().push(Locator {
                abs: abs.clone(),
                offset,
                len,
            });
        }
    }

    let kind = if single {
        let entry = manifest
            .files
            .first()
            .ok_or("single-file manifest must contain exactly one file")?;
        if manifest.files.len() != 1 {
            return Err("single-file share expects exactly one manifest entry".into());
        }

        let live = crate::fs::metadata(src).map_err(|e| format!("stat {}: {e}", src.display()))?;
        if live.kind != crate::fs::Kind::File {
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
            .and_then(|s| s.to_str())
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

    Ok(Shared {
        manifest_bytes,
        etag,
        kind,
        blocks,
        token,
    })
}
