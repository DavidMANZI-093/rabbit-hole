use clap::{Parser, Subcommand};
use std::{path::PathBuf, thread};

use crate::{
    protocol::manifest::common::{DEFAULT_BLOCK_SIZE, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE},
    services::fetch::{DEFAULT_TIMEOUT_MS, MAX_CONCURRENCY, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS},
};

#[derive(Parser)]
#[command(
    name = "rh",
    version,
    about = "rabbit-hole — fast, verified file transfer"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    Serve {
        path: PathBuf,

        #[arg(long, short, default_value_t = DEFAULT_BLOCK_SIZE, value_parser = clap::value_parser!(u32).range(MIN_BLOCK_SIZE as i64..=MAX_BLOCK_SIZE as i64))]
        block_size: u32,
        #[arg(long, short)]
        secure: bool,
        #[arg(long)]
        no_color: bool,
        #[arg(long, short)]
        verbose: bool,
    },
    Fetch {
        url: String,
        dest: PathBuf,

        #[arg(long, short, default_value_t = DEFAULT_TIMEOUT_MS, value_parser = clap::value_parser!(u32).range(MIN_TIMEOUT_MS as i64..=MAX_TIMEOUT_MS as i64))]
        timeout: u32,
        #[arg(long, short)]
        bearer: Option<String>,
        #[arg(long, short)]
        force: bool,
        #[arg(long, short, default_value_t = thread::available_parallelism().map_or(1, |n| n.get()).min(12) as u8, value_parser = clap::value_parser!(u8).range(1..=MAX_CONCURRENCY as i64))]
        concurrency: u8,
        #[arg(long)]
        no_color: bool,
        #[arg(long, short)]
        verbose: bool,
    },
    Check,
}
