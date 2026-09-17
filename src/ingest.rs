use std::path::PathBuf;

use crate::protocol::manifest::{
    Manifest,
    common::{MAX_BLOCK_SIZE, MIN_BLOCK_SIZE},
};

pub fn ingest(src: PathBuf, block_size: u32) -> Result<Manifest, String> {
    if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size) {
        return Err(format!(
            "block size must be {MIN_BLOCK_SIZE}..={MAX_BLOCK_SIZE}"
        ));
    }

    Ok(Manifest {
        block_size: 1024 * 1024,
        pool: [].to_vec(),
        files: [].to_vec(),
    })
}
