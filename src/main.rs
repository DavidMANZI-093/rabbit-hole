use clap::Parser;
use rabbit_hole::error;
use rabbit_hole::services::{cli::Cli, dispatch};

#[tokio::main]
async fn main() {
    if let Err(e) = dispatch::run((Cli::parse()).cmd).await {
        error!("{}", e);
        std::process::exit(1);
    };
}
