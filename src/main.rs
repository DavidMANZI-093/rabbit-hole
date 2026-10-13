use clap::Parser;
use rabbit_hole::error;
use rabbit_hole::services::{cli::Cli, dispatch};

#[tokio::main]
async fn main() {
    let command = Cli::parse().cmd;

    dispatch::run(command).await.map_err(|e| {
        error!("{}", e);
        std::process::exit(1);
    });
}
