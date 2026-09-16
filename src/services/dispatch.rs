use std::path::PathBuf;

use crate::services::cli::Cmd;

pub fn run(cmd: Cmd) {
    match cmd {
        Cmd::Share { path } => run_share(ShareOpts { path }),
        Cmd::Fetch { url, dest } => run_fetch(FetchOpts { url, dest }),
    }
}

fn run_share(opts: ShareOpts) {}
fn run_fetch(opts: FetchOpts) {}

struct ShareOpts {
    path: PathBuf,
}

struct FetchOpts {
    url: String,
    dest: PathBuf,
}
