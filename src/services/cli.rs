use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::protocol::manifest::common::{DEFAULT_BLOCK_SIZE, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE};

#[derive(Parser)]
#[command(
    name = "rh",
    version,
    about = "rabbit-hole, a better file sharing utility"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    Share {
        path: PathBuf,

        #[arg(long, short, default_value_t = DEFAULT_BLOCK_SIZE, value_parser = clap::value_parser!(u32).range(MIN_BLOCK_SIZE as i64..=MAX_BLOCK_SIZE as i64))]
        block_size: u32,
        #[arg(long, short)]
        verbose: bool,
    },
    Fetch {
        url: String,
        dest: PathBuf,

        #[arg(long, short)]
        verbose: bool,
    },
}
