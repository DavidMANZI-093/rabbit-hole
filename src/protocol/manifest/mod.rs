pub mod common;

#[derive(Clone)]
pub struct Manifest {
    pub block_size: u32,
    pub pool: Vec<[u8; 32]>,
    pub files: Vec<FileEntry>,
}

#[derive(Clone)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mode: Option<u32>,
    pub mtime_ns: Option<i64>,
    pub chunks: Vec<u32>,
}
