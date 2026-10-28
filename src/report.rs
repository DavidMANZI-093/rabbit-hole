use crate::info;
use crate::utils::format::format_bytes;
use crate::utils::tty::cyan;

// Pads `s` to 8 chars, then applies cyan — so ANSI codes never break alignment.
pub fn dlabel(s: &str) -> String {
    cyan(&format!("{s:<8}"))
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

pub fn print_fetch_summary(
    files: usize,
    bytes: u64,
    blocks: usize,
    failed: usize,
    retries: u64,
    dest: &std::path::Path,
) {
    info!("");
    info!("  {}  {}   {}", dlabel("files"), files, format_bytes(bytes),);
    info!(
        "  {}  {}   {} failed   {} retries",
        dlabel("blocks"),
        blocks,
        failed,
        retries,
    );
    info!("  {}  {}", dlabel("dest"), dest.display());
    info!("");
}
