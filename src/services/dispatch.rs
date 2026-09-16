use std::path::PathBuf;

use clap::builder::Str;

use crate::info;
use crate::services::cli::Cmd;
use crate::utils::log;

pub fn run(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Share { path, verbose } => run_share(ShareOpts { path, verbose }),
        Cmd::Fetch { url, dest, verbose } => run_fetch(FetchOpts { url, dest, verbose }),
    }
}

fn run_share(opts: ShareOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);

    info!("sharing content(s) at: {}", opts.path.display());
    Ok(())
}
fn run_fetch(opts: FetchOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);

    info!("fetching content(s) from: {}", opts.url);
    Ok(())
}

struct ShareOpts {
    path: PathBuf,
    verbose: bool,
}

struct FetchOpts {
    url: String,
    dest: PathBuf,
    verbose: bool,
}
