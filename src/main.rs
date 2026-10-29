use clap::Parser;
use rabbit_hole::cli::Cli;
use rabbit_hole::error;
use rabbit_hole::utils::{log, tty};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    log::set_verbose(cli.verbose);
    tty::init(cli.no_color);

    cli.cmd.execute().await.map_err(|e| {
        error!("{}", e);
        std::process::exit(1);
    });
}
