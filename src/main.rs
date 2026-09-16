use clap::Parser;
use rabbit_hole::error;
use rabbit_hole::services::{cli::Cli, dispatch};

fn main() {
    if let Err(e) = dispatch::run((Cli::parse()).cmd) {
        error!("{}", e);
        std::process::exit(1);
    };
}
