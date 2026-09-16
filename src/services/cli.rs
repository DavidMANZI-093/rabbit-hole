use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
