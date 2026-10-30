// Copyright (c) 2026 David MANZI
// SPDX-License-Identifier: MIT

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

// Cap preallocation so a claimed count cannot force a huge allocation before
// the bytes backing it have actually arrived.
const PREALLOC_CAP: u64 = 1 << 20;

struct Header {
    flags: u16,
    block_size: u32,
    root: Option<String>,
    file_count: u64,
    block_count: u64,
}

impl Header {
    fn decode(r: &mut Reader<'_>) -> Result<Self, ManifestError> {
        let magic = r.array::<4>()?;
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

        Ok(Self {
            flags,
            block_size,
            root: decode_root(r)?,
            file_count: r.uleb_limited(u64::from(MAX_FILES))?,
            block_count: r.uleb_limited(u64::from(MAX_BLOCKS))?,
        })
    }
}

fn decode_root(r: &mut Reader<'_>) -> Result<Option<String>, ManifestError> {
    let len = r.uleb_limited(MAX_PATH_LEN as u64)?;
    if len == 0 {
        return Ok(None);
    }

    let root = r.str(len as usize)?;
    validate_path(root)?;
    if root.contains('/') {
        return Err(ManifestError::BadPath(
            root.to_string(),
            "root must be a single path component",
        ));
    }
    Ok(Some(root.to_string()))
}

fn decode_pool(r: &mut Reader<'_>, count: u64) -> Result<Vec<[u8; 32]>, ManifestError> {
    let mut pool = Vec::with_capacity(capacity(count));
    for _ in 0..count {
        pool.push(r.array::<32>()?);
    }
    Ok(pool)
}

fn decode_files<'a>(r: &mut Reader<'a>, header: &Header) -> Result<Vec<FileEntry>, ManifestError> {
    let mut files = Vec::with_capacity(capacity(header.file_count));
    let mut seen: HashSet<&'a str> = HashSet::with_capacity(files.capacity());
    let root_prefix = header.root.as_ref().map(|root| format!("{root}/"));

    for _ in 0..header.file_count {
        let len = r.uleb_limited(MAX_PATH_LEN as u64)?;
        if len == 0 {
            return Err(ManifestError::OverLimit(0, MAX_PATH_LEN as u64));
        }

        let path = r.str(len as usize)?;
        validate_path(path)?;
        if let Some(prefix) = &root_prefix
            && !path.starts_with(prefix.as_str())
        {
            return Err(ManifestError::BadPath(
                path.to_string(),
                "outside manifest root",
            ));
        }
        if !seen.insert(path) {
            return Err(ManifestError::DuplicatePath(path.to_string()));
        }

        let size = r.uleb()?;
        let mode = if header.flags & FLAG_UNIX_MODE != 0 {
            Some(r.u32_le()?)
        } else {
            None
        };
        let mtime_ns = if header.flags & FLAG_MTIME != 0 {
            Some(r.i64_le()?)
        } else {
            None
        };
        let chunks = decode_chunks(r, size, header.block_size, header.block_count)?;

        files.push(FileEntry {
            path: path.to_string(),
            size,
            mode,
            mtime_ns,
            chunks,
        });
    }

    Ok(files)
}

fn decode_chunks(
    r: &mut Reader<'_>,
    size: u64,
    block_size: u32,
    block_count: u64,
) -> Result<Vec<u32>, ManifestError> {
    let count = r.uleb()?;
    if count != expected_chunks(size, block_size) {
        return Err(ManifestError::ChunkMismatch {
            got: count,
            size,
            bs: block_size,
        });
    }
    if count > u64::from(MAX_BLOCKS) {
        return Err(ManifestError::OverLimit(count, u64::from(MAX_BLOCKS)));
    }

    let mut chunks = Vec::with_capacity(capacity(count));
    for _ in 0..count {
        let idx = r.uleb()?;
        if idx >= block_count {
            return Err(ManifestError::BlockIndexOob(idx, block_count as usize));
        }
        chunks.push(idx as u32);
    }
    Ok(chunks)
}

fn capacity(n: u64) -> usize {
    n.min(PREALLOC_CAP) as usize
}

pub fn decode(input: &[u8]) -> Result<Manifest, ManifestError> {
    if input.len() > MAX_MANIFEST_BYTES as usize {
        return Err(ManifestError::TooLarge(MAX_MANIFEST_BYTES));
    }

    let mut r = Reader::new(input);
    let header = Header::decode(&mut r)?;
    let pool = decode_pool(&mut r, header.block_count)?;
    let files = decode_files(&mut r, &header)?;

    if r.position() != input.len() {
        return Err(ManifestError::Trailing(input.len() - r.position()));
    }

    Ok(Manifest {
        block_size: header.block_size,
        pool,
        files,
        root: header.root,
    })
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
        let mut bytes = bare_v2();
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

    fn bare_v2() -> Vec<u8> {
        let mut bytes = vec![b'R', b'H', b'B', b'1', 2, 0, 0, 0];
        bytes.extend_from_slice(&DEFAULT_BLOCK_SIZE.to_le_bytes());
        bytes
    }

    #[test]
    fn duplicate_path_rejected_on_decode() {
        let mut bytes = bare_v2();
        bytes.push(0); // no root
        bytes.push(2); // file count
        bytes.push(0); // pool count
        for _ in 0..2 {
            bytes.push(1);
            bytes.push(b'a');
            bytes.push(0); // size
            bytes.push(0); // chunks
        }
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicatePath(ref p) if p == "a"));
    }

    #[test]
    fn empty_file_path_rejected_on_decode() {
        let mut bytes = bare_v2();
        bytes.push(0); // no root
        bytes.push(1); // file count
        bytes.push(0); // pool count
        bytes.push(0); // path length
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, ManifestError::OverLimit(0, _)));
    }

    #[test]
    fn chunk_count_mismatch_rejected_on_decode() {
        let mut bytes = bare_v2();
        bytes.push(0); // no root
        bytes.push(1); // file count
        bytes.push(0); // pool count
        bytes.push(1); // path length
        bytes.push(b'a');
        bytes.push(1); // size: expects one chunk
        bytes.push(0); // chunks: claims none
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::ChunkMismatch {
                got: 0,
                size: 1,
                ..
            }
        ));
    }

    #[test]
    fn block_index_out_of_bounds_rejected_on_decode() {
        let mut bytes = bare_v2();
        bytes.push(0); // no root
        bytes.push(1); // file count
        bytes.push(0); // pool count
        bytes.push(1); // path length
        bytes.push(b'a');
        bytes.push(1); // size
        bytes.push(1); // chunk count
        bytes.push(0); // chunk index, but the pool is empty
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, ManifestError::BlockIndexOob(0, 0)));
    }
}
