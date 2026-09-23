use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::ingest::IngestStats;
use crate::utils::log::{bold, check_fail, check_ok, cyan, dim};
use crate::utils::progress::format_bytes;

// Pads `s` to 8 chars, then applies dim — so ANSI codes never break alignment.
pub fn dlabel(s: &str) -> String {
    cyan(&format!("{s:<8}"))
}

pub fn tunnel_spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", ""])
            .template(&format!("  {}  {{spinner}} {{msg}}", dlabel("tunnel")))
            .expect("valid spinner template"),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

pub fn prettify(s: &str) -> String {
    match s {
        "pending" => dim(s),
        "ok" => check_ok(),
        "failed" => check_fail(),
        _ => s.to_string(),
    }
}

// ---------- summary helpers (serve & fetch) ----------

pub fn print_ingest_summary(stats: &IngestStats) {
    let dedup_pct = if stats.blocks_total > 0 {
        (stats.blocks_total - stats.blocks_unique as u64) as f64 / stats.blocks_total as f64 * 100.0
    } else {
        0.0
    };
    eprintln!(
        "  {}  {}   {}",
        dlabel("files"),
        stats.files,
        format_bytes(stats.bytes),
    );
    eprintln!(
        "  {}  {}   {} unique   {dedup_pct:.1}% dedup",
        dlabel("blocks"),
        stats.blocks_total,
        stats.blocks_unique,
    );
    if stats.skipped > 0 {
        eprintln!("  {}  {} skipped", dlabel("skip"), stats.skipped);
    }
    eprintln!();
}

pub fn print_manifest_line(len: u64) {
    eprintln!("  {}  {}", dlabel("manifest"), format_bytes(len));
    eprintln!();
}

pub fn print_wan_and_fetch(lan: &str, wan: Option<&str>, code: Option<&str>, token: Option<&str>) {
    if let Some(wan_url) = wan {
        eprintln!("  {}  {}", dlabel("WAN"), bold(wan_url));
    }
    let target = wan.unwrap_or(lan);
    let fetch_cmd = match token {
        Some(_) => {
            if let Some(c) = code {
                format!("fetch with `rh fetch {c} <dest> --bearer <token>`")
            } else {
                format!("fetch with `rh fetch {target} <dest> --bearer <token>`")
            }
        }
        None => {
            if let Some(c) = code {
                format!("fetch with, `rh fetch {c} <dest>`")
            } else {
                format!("fetch with, `rh fetch {target} <dest>`")
            }
        }
    };
    if let Some(t) = token {
        eprintln!();
        eprintln!("  {}  {t}", dlabel("token"));
    }
    eprintln!();
    eprintln!("  {fetch_cmd}");
    eprintln!();
}

pub fn print_fetch_summary(
    files: usize,
    bytes: u64,
    blocks: usize,
    failed: usize,
    retries: u64,
    dest: &std::path::Path,
) {
    eprintln!();
    eprintln!("  {}  {}   {}", dlabel("files"), files, format_bytes(bytes),);
    eprintln!(
        "  {}  {}   {} failed   {} retries",
        dlabel("blocks"),
        blocks,
        failed,
        retries,
    );
    eprintln!("  {}  {}", dlabel("dest"), dest.display());
    eprintln!();
}
