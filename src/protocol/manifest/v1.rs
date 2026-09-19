use crate::protocol::manifest::{Manifest, common::encode_uleb};

const MAGIC: [u8; 4] = *b"RHB1";
const VERSION: u16 = 1;
const FLAG_UNIX_MODE: u16 = 1 << 0;
const FLAG_MTIME: u16 = 1 << 1;

pub fn encode(m: &Manifest) -> Vec<u8> {
    m.validate()
        .map_err(|e| format!("invalid manifest: {}", e.to_string()));

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
