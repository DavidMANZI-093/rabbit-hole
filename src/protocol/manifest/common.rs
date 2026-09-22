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
    TooLarge(u32),
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

impl std::error::Error for ManifestError {}

// ---------- unsigned LEB128 ----------
// 7 payload bits per byte, continuation at msb; minimal-length form required on decode

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

fn encoded_uleb_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], ManifestError> {
        let end = self.pos.checked_add(n).ok_or(ManifestError::Overflow)?;
        if end > self.buf.len() {
            return Err(ManifestError::Truncated {
                at: self.pos,
                need: n,
            });
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn u16_le(&mut self) -> Result<u16, ManifestError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().map_err(
            |_| ManifestError::Truncated {
                at: self.pos,
                need: 2,
            },
        )?))
    }

    pub fn u32_le(&mut self) -> Result<u32, ManifestError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(
            |_| ManifestError::Truncated {
                at: self.pos,
                need: 4,
            },
        )?))
    }

    pub fn uleb(&mut self) -> Result<u64, ManifestError> {
        let start = self.pos;
        let mut val: u64 = 0;
        let mut shift = 0u32;
        loop {
            if self.pos >= self.buf.len() {
                return Err(ManifestError::Truncated { at: start, need: 1 });
            }
            if shift >= 70 {
                return Err(ManifestError::Overflow);
            }
            let b = self.buf[self.pos];
            self.pos += 1;
            if shift >= 64 && (b & 0x7f) != 0 {
                return Err(ManifestError::Overflow);
            }
            val |= ((b & 0x7f) as u64) << shift;
            let done = b & 0x80 == 0;
            shift += 7;
            if done {
                if self.pos - start != encoded_uleb_len(val) {
                    return Err(ManifestError::NonCanonical);
                }
                return Ok(val);
            }
            if self.pos - start > 10 {
                return Err(ManifestError::Overflow);
            }
        }
    }

    pub fn i64_le(&mut self) -> Result<i64, ManifestError> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().map_err(
            |_| ManifestError::Truncated {
                at: self.pos,
                need: 8,
            },
        )?))
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

pub fn hex_encode(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- validate_path ---

    #[test]
    fn empty_path_is_rejected() {
        assert!(validate_path("").is_err());
    }

    #[test]
    fn leading_slash_is_rejected() {
        assert!(validate_path("/foo/bar").is_err());
    }

    #[test]
    fn trailing_slash_is_rejected() {
        assert!(validate_path("foo/bar/").is_err());
    }

    #[test]
    fn double_slash_is_rejected() {
        assert!(validate_path("foo//bar").is_err());
    }

    #[test]
    fn dot_component_is_rejected() {
        assert!(validate_path("foo/./bar").is_err());
    }

    #[test]
    fn dotdot_component_is_rejected() {
        assert!(validate_path("foo/../bar").is_err());
    }

    #[test]
    fn null_byte_is_rejected() {
        assert!(validate_path("foo\0bar").is_err());
    }

    #[test]
    fn simple_filename_is_accepted() {
        assert!(validate_path("hello.txt").is_ok());
    }

    #[test]
    fn nested_path_is_accepted() {
        assert!(validate_path("a/b/c/d.txt").is_ok());
    }

    // --- expected_chunks ---

    #[test]
    fn zero_size_has_no_chunks() {
        assert_eq!(expected_chunks(0, 1024), 0);
    }

    #[test]
    fn exact_block_size_is_one_chunk() {
        assert_eq!(expected_chunks(4096, 4096), 1);
    }

    #[test]
    fn one_byte_over_block_size_is_two_chunks() {
        assert_eq!(expected_chunks(4097, 4096), 2);
    }

    #[test]
    fn large_file_chunk_count_is_correct() {
        let size = 10 * 1024 * 1024u64; // 10 MiB
        let bs = 1024 * 1024u32; // 1 MiB
        assert_eq!(expected_chunks(size, bs), 10);
    }

    // --- hex encode / decode ---

    #[test]
    fn hex_roundtrip_all_zeros() {
        let bytes = [0u8; 32];
        let encoded = hex_encode(&bytes);
        assert_eq!(encoded, "0".repeat(64));
        assert_eq!(hex_decode32(&encoded).unwrap(), bytes);
    }

    #[test]
    fn hex_roundtrip_all_ones() {
        let bytes = [0xffu8; 32];
        let encoded = hex_encode(&bytes);
        assert_eq!(encoded, "ff".repeat(32));
        assert_eq!(hex_decode32(&encoded).unwrap(), bytes);
    }

    #[test]
    fn hex_decode_rejects_wrong_length() {
        assert!(hex_decode32("abc").is_err());
        assert!(hex_decode32(&"a".repeat(63)).is_err());
        assert!(hex_decode32(&"a".repeat(65)).is_err());
    }

    #[test]
    fn hex_decode_rejects_invalid_digit() {
        let bad = "g".repeat(64);
        assert!(hex_decode32(&bad).is_err());
    }

    // --- uleb128 roundtrip ---

    fn uleb_roundtrip(v: u64) -> u64 {
        let mut buf = Vec::new();
        encode_uleb(v, &mut buf);
        Reader::new(&buf).uleb().expect("decode failed")
    }

    #[test]
    fn uleb_zero_roundtrips() {
        assert_eq!(uleb_roundtrip(0), 0);
    }

    #[test]
    fn uleb_127_roundtrips() {
        assert_eq!(uleb_roundtrip(127), 127);
    }

    #[test]
    fn uleb_128_roundtrips() {
        assert_eq!(uleb_roundtrip(128), 128);
    }

    #[test]
    fn uleb_max_u64_roundtrips() {
        assert_eq!(uleb_roundtrip(u64::MAX), u64::MAX);
    }

    #[test]
    fn non_canonical_uleb_is_rejected() {
        // 0x00 encoded as two bytes (0x80, 0x00) is non-canonical
        let non_canonical = vec![0x80u8, 0x00];
        assert!(Reader::new(&non_canonical).uleb().is_err());
    }
}
