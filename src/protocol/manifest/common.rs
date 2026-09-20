pub const DEFAULT_BLOCK_SIZE: u32 = 1024 * 1024; //  1,048,576 bytes or ~1MB
pub const MIN_BLOCK_SIZE: u32 = 4096; // 4,096 bytes or ~4KB
pub const MAX_BLOCK_SIZE: u32 = 64 * 1024 * 1024; // 67,108,864 bytes or ~64MB
pub const MAX_PATH_LEN: usize = 4096; // 4,096 bytes or ~4KB 
pub const MAX_FILES: u32 = 1_000_000;
pub const MAX_BLOCKS: u32 = 32_000_000;
pub const MAX_MANIFEST_BYTES: u32 = 256 * 1024 * 1024; // 268,435,456 bytes or ~256MB

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    BadMagic,
    UnsupportedVersion(u16),
    UnknownFlags(u16),
    BadBlockSize(u32),
    Truncated { at: usize, need: usize },
    TooLarge(usize),
    NonCanonical,
    Overflow,
    OverLimit(u64, u64),
    BadPath(String, &'static str),
    DuplicatePath(String),
    ChunkMismatch { got: u64, size: u64, bs: u32 },
    BlockIndexOob(u64, usize),
    Trailing(usize),
    BadUtf8,
    BadHex(&'static str),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "bad magic, expected RHB1"),
            Self::UnsupportedVersion(v) => {
                write!(
                    f,
                    "unsupported manifest version {v} (upgrade sender/receiver)"
                )
            }
            Self::UnknownFlags(v) => {
                write!(f, "unknown flags {v:#06x} (upgrade sender/receiver)")
            }
            Self::BadBlockSize(v) => write!(f, "invalid block size {v}"),
            Self::Truncated { at, need } => {
                write!(f, "truncated manifest (need {need} bytes at offset {at})")
            }
            Self::TooLarge(v) => write!(f, "manifest exceeds {v} bytes"),
            Self::NonCanonical => write!(f, "non-canonical variable-length integer"),
            Self::Overflow => write!(f, "variable-length integer overflow"),
            Self::OverLimit(a, b) => write!(f, "value {a} exceeds limit {b}"),
            Self::BadPath(s, r) => write!(f, "invalid path {s:?}: {r}"),
            Self::DuplicatePath(s) => write!(f, "duplicate path {s:?}"),
            Self::ChunkMismatch { got, size, bs } => write!(
                f,
                "chunk count {got} inconsistent with size {size} and block size {bs}"
            ),
            Self::BlockIndexOob(a, b) => write!(f, "block index {a} out of bounds ({b} in pool)"),
            Self::Trailing(n) => write!(f, "trailing {n} unexpected bytes"),
            Self::BadUtf8 => write!(f, "invalid UTF8 path"),
            Self::BadHex(r) => write!(f, "invalid hex: {r}"),
        }
    }
}

// impl std::error::Error for ManifestError {}

// ---------- unsigned LEB128 variable-length compression ----------
//
// 7 payload bits per byte plus one continuation bit.
// Minimal-length form is mandatory on decode.

pub fn encode_uleb(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
            out.push(b);
        } else {
            out.push(b);
            return;
        }
    }
}

// ---------- paths & chunk helpers ----------

pub fn validate_path(p: &str) -> Result<(), ManifestError> {
    if p.len() > MAX_PATH_LEN {
        return Err(ManifestError::OverLimit(
            p.len() as u64,
            MAX_PATH_LEN as u64,
        ));
    }
    if p.is_empty() {
        return Err(ManifestError::BadPath(p.into(), "empty"));
    }
    if p.starts_with('/') || p.ends_with('/') || p.contains("//") {
        return Err(ManifestError::BadPath(
            p.into(),
            "must be relative without empty components",
        ));
    }
    if p.contains('\0') {
        return Err(ManifestError::BadPath(p.into(), "NUL byte"));
    }
    for comp in p.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return Err(ManifestError::BadPath(
                p.into(),
                "dot/dotdot/empty component",
            ));
        }
    }
    Ok(())
}

pub fn expected_chunks(size: u64, block_size: u32) -> u64 {
    if size == 0 {
        0
    } else {
        size.div_ceil(block_size as u64)
    }
}

pub fn hex_decode32(s: &str) -> Result<[u8; 32], ManifestError> {
    if s.len() != 64 {
        return Err(ManifestError::BadHex("need 64 hex chars"));
    }
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    for i in 0..32 {
        let hi = hexval(b[2 * i]).ok_or(ManifestError::BadHex("bad digit"))?;
        let lo = hexval(b[2 * i + 1]).ok_or(ManifestError::BadHex("bad digit"))?;
        out[i] = hi << 4 | lo;
    }
    Ok(out)
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
