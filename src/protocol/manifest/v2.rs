use std::collections::HashSet;

use crate::protocol::manifest::{
    FileEntry, Manifest,
    common::{
        MAX_BLOCK_SIZE, MAX_BLOCKS, MAX_FILES, MAX_MANIFEST_BYTES, MAX_PATH_LEN, MIN_BLOCK_SIZE,
        ManifestError, Reader, encode_uleb, expected_chunks, validate_path,
    },
    v1::MAGIC,
};

const VERSION: u16 = 2;
const FLAG_UNIX_MODE: u16 = 1 << 0;
const FLAG_MTIME: u16 = 1 << 1;
const KNOWN_FLAGS: u16 = FLAG_UNIX_MODE | FLAG_MTIME;

pub fn encode(m: &Manifest) -> Vec<u8> {
    debug_assert!(m.validate().is_ok(), "encode called with invalid manifest");

    let mut out = Vec::with_capacity(16 + m.pool.len() * 32);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let mut flags = 0u16;
    if m.files.iter().any(|e| e.mode.is_some()) {
        flags |= FLAG_UNIX_MODE;
    }
    if m.files.iter().any(|e| e.mtime_ns.is_some()) {
        flags |= FLAG_MTIME;
    }
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&m.block_size.to_le_bytes());
    match &m.root {
        Some(r) => {
            encode_uleb(r.len() as u64, &mut out);
            out.extend_from_slice(r.as_bytes());
        }
        None => encode_uleb(0, &mut out),
    }
    encode_uleb(m.files.len() as u64, &mut out);
    encode_uleb(m.pool.len() as u64, &mut out);
    for b in &m.pool {
        out.extend_from_slice(b);
    }

    for f in &m.files {
        let pb = f.path.as_bytes();
        encode_uleb(pb.len() as u64, &mut out);
        out.extend_from_slice(pb);
        encode_uleb(f.size, &mut out);
        if flags & FLAG_UNIX_MODE != 0 {
            out.extend_from_slice(&f.mode.unwrap_or(0o644).to_le_bytes());
        }
        if flags & FLAG_MTIME != 0 {
            out.extend_from_slice(&f.mtime_ns.unwrap_or(0).to_le_bytes());
        }
        encode_uleb(f.chunks.len() as u64, &mut out);
        for c in &f.chunks {
            encode_uleb(*c as u64, &mut out);
        }
    }
    out
}

pub fn decode(input: &[u8]) -> Result<Manifest, ManifestError> {
    if input.len() > MAX_MANIFEST_BYTES as usize {
        return Err(ManifestError::TooLarge(MAX_MANIFEST_BYTES));
    }

    let mut r = Reader::new(input);
    let magic: [u8; 4] = r.take(4).and_then(|s| {
        s.try_into().map_err(|_| ManifestError::Truncated {
            at: r.position(),
            need: 4,
        })
    })?;
    if magic != MAGIC {
        return Err(ManifestError::BadMagic);
    }

    let version = r.u16_le()?;
    if version != VERSION {
        return Err(ManifestError::UnsupportedVersion(version));
    }

    let flags = r.u16_le()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(ManifestError::UnknownFlags(flags));
    }

    let block_size = r.u32_le()?;
    if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size) {
        return Err(ManifestError::BadBlockSize(block_size));
    }

    let root_len = r.uleb()?;
    if root_len > MAX_PATH_LEN as u64 {
        return Err(ManifestError::OverLimit(root_len, MAX_PATH_LEN as u64));
    }
    let root = if root_len == 0 {
        None
    } else {
        let s =
            std::str::from_utf8(r.take(root_len as usize)?).map_err(|_| ManifestError::BadUtf8)?;
        validate_path(s)?;
        if s.contains('/') {
            return Err(ManifestError::BadPath(
                s.to_string(),
                "root must be a single path component",
            ));
        }
        Some(s.to_string())
    };

    let file_count = r.uleb()?;
    if file_count > MAX_FILES as u64 {
        return Err(ManifestError::OverLimit(file_count, MAX_FILES as u64));
    }

    let block_count = r.uleb()?;
    if block_count > MAX_BLOCKS as u64 {
        return Err(ManifestError::OverLimit(block_count, MAX_BLOCKS as u64));
    }

    let mut pool = Vec::with_capacity(block_count.min(1 << 20) as usize);
    for _ in 0..block_count {
        let mut h = [0u8; 32];
        h.copy_from_slice(r.take(32)?);
        pool.push(h);
    }

    let mut files = Vec::with_capacity(file_count.min(1 << 20) as usize);
    let mut seen = HashSet::with_capacity(files.capacity());

    for _ in 0..file_count {
        let plen = r.uleb()?;
        if plen == 0 || plen > MAX_PATH_LEN as u64 {
            return Err(ManifestError::OverLimit(plen, MAX_PATH_LEN as u64));
        }

        let path =
            std::str::from_utf8(r.take(plen as usize)?).map_err(|_| ManifestError::BadUtf8)?;
        validate_path(path)?;
        if !seen.insert(path.to_string()) {
            return Err(ManifestError::DuplicatePath(path.to_string()));
        }

        let size = r.uleb()?;

        let mode = if flags & FLAG_UNIX_MODE != 0 {
            Some(r.u32_le()?)
        } else {
            None
        };

        let mtime_ns = if flags & FLAG_MTIME != 0 {
            Some(r.i64_le()?)
        } else {
            None
        };

        let chunk_count = r.uleb()?;
        if chunk_count != expected_chunks(size, block_size) {
            return Err(ManifestError::ChunkMismatch {
                got: chunk_count,
                size,
                bs: block_size,
            });
        }
        if chunk_count > MAX_BLOCKS as u64 {
            return Err(ManifestError::OverLimit(chunk_count, MAX_BLOCKS as u64));
        }
        let mut chunks = Vec::with_capacity(chunk_count.min(1 << 20) as usize);
        for _ in 0..chunk_count {
            let idx = r.uleb()?;
            if idx >= block_count {
                return Err(ManifestError::BlockIndexOob(idx, pool.len()));
            }
            chunks.push(idx as u32);
        }

        files.push(FileEntry {
            path: path.to_string(),
            size,
            mode,
            mtime_ns,
            chunks,
        });
    }

    if r.position() != input.len() {
        return Err(ManifestError::Trailing(input.len() - r.position()));
    }

    let manifest = Manifest {
        block_size,
        pool,
        files,
        root,
    };
    // Root/file-prefix consistency, same rules as Manifest::validate.
    if let Some(rr) = &manifest.root {
        let prefix = format!("{rr}/");
        for f in &manifest.files {
            if !f.path.starts_with(prefix.as_str()) {
                return Err(ManifestError::BadPath(
                    f.path.clone(),
                    "outside manifest root",
                ));
            }
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::manifest::common::DEFAULT_BLOCK_SIZE;

    fn rooted() -> Manifest {
        Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: Some("photos".to_string()),
            pool: vec![],
            files: vec![],
        }
    }

    #[test]
    fn rooted_roundtrips() {
        let m = rooted();
        let bytes = encode(&m);
        assert_eq!(crate::protocol::manifest::peek_version(&bytes).unwrap(), 2);
        assert_eq!(decode(&bytes).expect("decode failed"), m);
    }

    #[test]
    fn rootless_via_v2_codec_roundtrips() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: None,
            pool: vec![],
            files: vec![],
        };
        assert_eq!(decode(&encode(&m)).expect("decode failed"), m);
    }

    #[test]
    fn rooted_with_metadata_roundtrips() {
        let m = Manifest {
            block_size: DEFAULT_BLOCK_SIZE,
            root: Some("photos".to_string()),
            pool: vec![[7u8; 32]],
            files: vec![FileEntry {
                path: "photos/a.txt".to_string(),
                size: DEFAULT_BLOCK_SIZE as u64,
                mode: Some(0o644),
                mtime_ns: Some(1_700_000_000_000_000_000),
                chunks: vec![0],
            }],
        };
        assert_eq!(decode(&encode(&m)).expect("decode failed"), m);
    }

    #[test]
    fn v1_codec_rejects_v2_bytes_with_upgrade_error() {
        // Old readers fail closed at the version field, before parsing structure.
        let bytes = encode(&rooted());
        let err = crate::protocol::manifest::v1::decode(&bytes).unwrap_err();
        assert!(matches!(err, ManifestError::UnsupportedVersion(2)));
    }

    #[test]
    fn overlong_root_rejected() {
        let mut bytes = encode(&rooted());
        // root_len lives at offset 4+2+2+4 = 12; overwrite with huge uleb.
        bytes[12] = 0x80;
        bytes[13] = 0x80;
        bytes[14] = 0x80;
        bytes[15] = 0x80;
        bytes[16] = 0x10;
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn file_outside_root_rejected_on_decode() {
        // Hand-built v2: root "photos", one empty file "other/a.txt".
        let mut bytes = vec![b'R', b'H', b'B', b'1', 2, 0, 0, 0];
        bytes.extend_from_slice(&(DEFAULT_BLOCK_SIZE).to_le_bytes());
        bytes.push(6);
        bytes.extend_from_slice(b"photos");
        bytes.push(1); // file count
        bytes.push(0); // pool count
        bytes.push(11); // "other/a.txt".len()
        bytes.extend_from_slice(b"other/a.txt");
        bytes.push(0); // size
        bytes.push(0); // chunks
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, ManifestError::BadPath(_, _)));
    }
}
