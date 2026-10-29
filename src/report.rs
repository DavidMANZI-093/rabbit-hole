use std::path::Path;

use crate::{
    info,
    services::{cloudflared, fetch::FetchStats},
    utils::{
        format::format_bytes,
        progress::FetchSnapshot,
        tty::{bold, green, purple, red, yellow},
    },
};

pub fn print_cloudflared_status(status: &cloudflared::Status) {
    let Some(line) = status.line.as_deref() else {
        info!(
            "cloudflared: {} (see: {})",
            bold(red("missing").as_str()),
            cloudflared::SOURCE_PAGE
        );
        return;
    };

    let src = if status.bundled { "bundled" } else { "in PATH" };
    match status.verdict {
        cloudflared::Verdict::Supported(_) => info!(
            "cloudflared: {} ({src}) - {line}",
            bold(green("available").as_str())
        ),
        cloudflared::Verdict::TooOld(v) => info!(
            "cloudflared: {}, need >= {} (have {v}) (see {})",
            bold(yellow("too old").as_str()),
            cloudflared::MIN,
            cloudflared::SOURCE_PAGE
        ),
        cloudflared::Verdict::Unknown => info!(
            "cloudflared: {} ({src}) - {line}",
            bold(purple("unknown").as_str())
        ),
    }
}

pub fn print_serve_details(
    lan: &str,
    wan: Option<&str>,
    code: Option<&str>,
    token: Option<&str>,
    show_local: bool,
) {
    if let Some(wan_url) = wan {
        let tab_n_align = |name: &str, value: &str| -> String { format!("    {name:>7}: {value}") };
        info!("{}", tab_n_align("url", wan_url));
        if show_local {
            info!("{}", tab_n_align("local", lan));
        }
        match token {
            Some(t) => {
                info!("{}", tab_n_align("secured", "true"));
                if let Some(c) = code {
                    info!("{}", tab_n_align("code", c));
                }
                info!("{}", tab_n_align("token", t));
            }
            None => {
                info!(
                    "{}",
                    tab_n_align("secured", "false (secure with '--secure' flag)")
                );
                if let Some(c) = code {
                    info!("{}", tab_n_align("code", c));
                }
            }
        }
    }

    let target = wan.unwrap_or(lan);
    let fetch_cmd = match token {
        Some(_) => {
            if let Some(c) = code {
                format!("`rh fetch {c} <dest> --bearer <token>`")
            } else {
                format!("`rh fetch {target} <dest> --bearer <token>`")
            }
        }
        None => {
            if let Some(c) = code {
                format!("`rh fetch {c} <dest>`")
            } else {
                format!("`rh fetch {target} <dest>`")
            }
        }
    };

    info!("");
    info!("fetch with, {}", fetch_cmd);
    info!("");
    info!("press Ctrl-C to stop serving.");
}

pub struct FetchSummary {
    ok: bool,
    files: usize,
    bytes: u64,
    blocks_done: u64,
    blocks_total: u64,
    retries: u64,
    failed: u64,
}

impl FetchSummary {
    pub fn ok(stats: &FetchStats) -> Self {
        let blocks = stats.blocks as u64;
        Self {
            ok: true,
            files: stats.files,
            bytes: stats.bytes,
            blocks_done: blocks,
            blocks_total: blocks,
            retries: stats.retries,
            failed: 0,
        }
    }

    pub fn incomplete(snapshot: &FetchSnapshot) -> Self {
        Self {
            ok: false,
            files: snapshot.files,
            bytes: snapshot.total_bytes,
            blocks_done: snapshot.blocks_done,
            blocks_total: snapshot.blocks_total,
            retries: snapshot.retries,
            failed: snapshot.failed,
        }
    }
}

pub fn print_fetch_summary(summary: &FetchSummary, dest: &Path) {
    let status = if summary.ok { "done" } else { "failed" };
    let line = summary_line(summary, dest);
    let rest = line.strip_prefix(status).unwrap_or(&line);
    let status = if summary.ok {
        green(status)
    } else {
        red(status)
    };
    info!("");
    info!("{status}{rest}");
    info!("");
}

fn summary_line(summary: &FetchSummary, dest: &Path) -> String {
    let status = if summary.ok { "done" } else { "failed" };
    let mut line = format!("{status}. ");

    if summary.blocks_total == 0 {
        line.push_str("0 blocks");
    } else {
        // Truncated, not rounded: 15/17 reads as 88.23%, not 88.24%.
        let pct = summary
            .blocks_done
            .saturating_mul(10_000)
            .checked_div(summary.blocks_total)
            .map(|q| q as f64 / 100.0)
            .unwrap_or(0.0);
        line.push_str(&format!(
            "{pct:.2}% ({}/{} blocks",
            summary.blocks_done, summary.blocks_total
        ));
        if summary.failed > 0 {
            line.push_str(&format!(", {} failed", summary.failed));
        }
        line.push(')');
    }

    let noun = if summary.files == 1 { "file" } else { "files" };
    line.push_str(&format!(
        ", {} {noun}, {}",
        summary.files,
        format_bytes(summary.bytes)
    ));
    if summary.retries > 0 {
        line.push_str(&format!(", {} retries", summary.retries));
    }
    line.push_str(&format!(" in {}", dest.display()));
    if !summary.ok {
        line.push_str(" — incomplete");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(ok: bool, files: usize, bytes: u64, done: u64, total: u64) -> FetchSummary {
        FetchSummary {
            ok,
            files,
            bytes,
            blocks_done: done,
            blocks_total: total,
            retries: 0,
            failed: 0,
        }
    }

    #[test]
    fn success_line_has_percent_files_and_dest() {
        let line = summary_line(&summary(true, 2, 12, 17, 17), Path::new("/tmp/out"));
        assert_eq!(
            line,
            "done. 100.00% (17/17 blocks), 2 files, 12 B in /tmp/out"
        );
    }

    #[test]
    fn failure_line_is_marked_incomplete() {
        let mut s = summary(false, 2, 1024, 15, 17);
        s.failed = 2;
        let line = summary_line(&s, Path::new("/tmp/out"));
        assert_eq!(
            line,
            "failed. 88.23% (15/17 blocks, 2 failed), 2 files, 1.0 KiB in /tmp/out — incomplete"
        );
    }

    #[test]
    fn zero_blocks_omits_percent() {
        let line = summary_line(&summary(true, 1, 0, 0, 0), Path::new("/tmp/out"));
        assert_eq!(line, "done. 0 blocks, 1 file, 0 B in /tmp/out");
    }

    #[test]
    fn retries_are_reported_when_present() {
        let mut s = summary(true, 1, 12, 1, 1);
        s.retries = 3;
        let line = summary_line(&s, Path::new("/tmp/out"));
        assert_eq!(
            line,
            "done. 100.00% (1/1 blocks), 1 file, 12 B, 3 retries in /tmp/out"
        );
    }
}
