// Copyright (c) 2026 David MANZI
// SPDX-License-Identifier: MIT

use clap::{Parser, Subcommand};
use std::{path::PathBuf, thread};

use crate::{
    protocol::manifest::common::{MAX_BLOCK_SIZE, MIN_BLOCK_SIZE},
    services::{
        edge::COMPILED_EDGE_BASE,
        fetch::{DEFAULT_TIMEOUT_MS, MAX_CONCURRENCY, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS},
    },
};

#[derive(Parser)]
#[command(
    name = "rh",
    version,
    max_term_width = 80,
    about = "rabbit-hole is a file transfer utility for sharing files and folders as-is (no archiving or repackaging) directly; with no middleman holding or staging; and per-block end-to-end hash verification."
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,

    /// Disable colored output.
    #[arg(long)]
    pub no_color: bool,

    /// Enable debug logging.
    #[arg(short = 'v', long = "verbose", global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Serve a file or directory over LAN or a public tunnel.
    Serve {
        /// File or directory to share.
        ///
        /// Directory contents are shared as-is; symlinks are skipped or refused.
        path: PathBuf,

        /// Block size for hashing and transfer (4 KiB..=64 MiB; K/M/G = binary).
        ///
        /// Smaller blocks improve deduplication and resume granularity; larger blocks
        /// reduce request count.
        #[arg(long, short, value_name = "SIZE", default_value = "1MiB", value_parser = parse_block_size)]
        block_size: u32,

        /// Generate and require a bearer token (authentication only).
        ///
        /// The token is printed with the serve details. It does not encrypt LAN
        /// traffic; the public tunnel is HTTPS.
        #[arg(long, short)]
        secure: bool,

        /// Override the short-code service URL used to claim a code (advanced).
        ///
        /// Normally configured a free default. Only needed for a self-hosted or
        /// alternate service.
        #[arg(long, short, value_name = "URL", default_value = COMPILED_EDGE_BASE, hide_default_value = true)]
        edge: String,

        /// Skip the public tunnel and short code; use the direct local URL.
        ///
        /// The listener still binds to all interfaces; use a firewall if you need
        /// interface-level restrictions.
        #[arg(long, short)]
        local: bool,
    },

    /// Fetch a shared file or directory using a short code or direct URL.
    Fetch {
        /// 6-character hex short code or full HTTP(S) URL.
        ///
        /// Short codes are resolved through the configured short-code service;
        /// direct URLs bypass it.
        code_or_url: String,

        /// Directory to receive files (created if needed).
        ///
        /// Existing matching files are reused; conflicting targets abort unless
        /// --force is passed.
        dest: PathBuf,

        /// Per-request timeout in milliseconds (15000..=120000).
        ///
        /// Applies to the manifest and each block request; retry delays are
        /// additional.
        #[arg(long, short, default_value_t = DEFAULT_TIMEOUT_MS, value_parser = clap::value_parser!(u32).range(MIN_TIMEOUT_MS as i64..=MAX_TIMEOUT_MS as i64))]
        timeout: u32,

        /// Bearer token printed by 'rh serve --secure'.
        #[arg(long, short)]
        bearer: Option<String>,

        /// Allow writing over files that differ from the share.
        ///
        /// Only conflicting target files are touched; matching blocks and
        /// unrelated files are preserved.
        #[arg(long, short)]
        force: bool,

        /// Concurrent block downloads (1..=12).
        ///
        /// Defaults to the CPU count capped at 12. Higher values can help
        /// high-latency links but increase memory and bandwidth pressure.
        #[arg(long, short, default_value_t = thread::available_parallelism().map_or(1, |n| n.get()).min(12) as u8, value_parser = clap::value_parser!(u8).range(1..=MAX_CONCURRENCY as i64))]
        concurrency: u8,

        /// Override the short-code service URL used to resolve a code (advanced).
        ///
        /// Normally configured at build time. Direct URLs do not need it.
        #[arg(long, short, value_name = "URL", default_value = COMPILED_EDGE_BASE, hide_default_value = true)]
        edge: String,
    },

    /// Check cloudflared installation and compatibility for public tunneling.
    Check,
}

fn parse_block_size(raw: &str) -> Result<u32, String> {
    let compact: String = raw.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact.is_empty() {
        return Err("block size is empty".into());
    }

    let split = compact
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(compact.len());
    let (digits, suffix) = compact.split_at(split);
    if digits.is_empty() {
        return Err(format!("invalid block size {raw:?}: missing number"));
    }

    let n: u64 = digits
        .parse()
        .map_err(|_| format!("invalid block size {raw:?}"))?;
    let multiplier: u64 = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kib" => 1024,
        "m" | "mib" => 1024 * 1024,
        "g" | "gib" => 1024 * 1024 * 1024,
        "kb" | "mb" | "gb" => {
            return Err(format!(
                "invalid block size {raw:?}: use KiB/MiB/GiB (binary) or raw bytes"
            ));
        }
        _ => {
            return Err(format!(
                "invalid block size {raw:?}: unknown suffix {suffix:?}"
            ));
        }
    };
    let bytes: u64 = n
        .checked_mul(multiplier)
        .ok_or_else(|| format!("invalid block size {raw:?}: too large"))?;

    if !(MIN_BLOCK_SIZE as u64..=MAX_BLOCK_SIZE as u64).contains(&bytes) {
        return Err(format!(
            "invalid block size {raw:?}: must be {MIN_BLOCK_SIZE}..={MAX_BLOCK_SIZE} bytes (4 KiB..=64 MiB)"
        ));
    }
    u32::try_from(bytes).map_err(|_| format!("invalid block size {raw:?}: too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_bytes() {
        assert_eq!(parse_block_size("4096"), Ok(4096));
        assert_eq!(parse_block_size("1048576"), Ok(1024 * 1024));
    }

    #[test]
    fn parses_binary_suffixes() {
        assert_eq!(parse_block_size("4K"), Ok(4096));
        assert_eq!(parse_block_size("4KiB"), Ok(4096));
        assert_eq!(parse_block_size("1M"), Ok(1024 * 1024));
        assert_eq!(parse_block_size("1MiB"), Ok(1024 * 1024));
        assert_eq!(parse_block_size("64MiB"), Ok(64 * 1024 * 1024));
        assert_eq!(parse_block_size("1 MiB"), Ok(1024 * 1024));
    }

    #[test]
    fn rejects_decimal_si_suffixes() {
        assert!(parse_block_size("1MB").is_err());
        assert!(parse_block_size("1KB").is_err());
    }

    #[test]
    fn rejects_out_of_range_and_invalid_sizes() {
        assert!(parse_block_size("3KiB").is_err());
        assert!(parse_block_size("65MiB").is_err());
        assert!(parse_block_size("0").is_err());
        assert!(parse_block_size("").is_err());
        assert!(parse_block_size("MiB").is_err());
        assert!(parse_block_size("1.5MiB").is_err());
    }

    #[test]
    fn parses_default_from_cli() {
        let cli = Cli::try_parse_from(["rh", "serve", "/tmp"]).expect("parse default");
        let Command::Serve { block_size, .. } = cli.cmd else {
            panic!("expected serve");
        };
        assert_eq!(block_size, 1024 * 1024);
    }
}
