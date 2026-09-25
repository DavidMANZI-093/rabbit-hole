use std::fmt;

pub const MIN: CalVer = CalVer {
    year: 2026,
    minor: 5,
    patch: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CalVer {
    pub year: u16,
    pub minor: u16,
    pub patch: u16,
}

impl fmt::Display for CalVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.year, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Supported(CalVer),
    TooOld(CalVer),
    Unknown,
}

pub fn classify(v: Option<CalVer>) -> Verdict {
    match v {
        Some(c) if c >= MIN => Verdict::Supported(c),
        Some(c) => Verdict::TooOld(c),
        None => Verdict::Unknown,
    }
}

pub fn parse_version(s: &str) -> Option<CalVer> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit()
            && let Some(v) = try_triple_at(b, i)
        {
            return Some(v);
        }
        i += 1;
    }
    None
}

fn try_triple_at(b: &[u8], i: usize) -> Option<CalVer> {
    let mut j = i;
    let year = take_digits(b, &mut j, 4, 4)?;
    if b.get(j) != Some(&b'.') {
        return None;
    }
    j += 1;
    let minor = take_digits(b, &mut j, 1, 3)?;
    if b.get(j) != Some(&b'.') {
        return None;
    }
    j += 1;
    let patch = take_digits(b, &mut j, 1, 3)?;
    Some(CalVer {
        year: year as u16,
        minor: minor as u16,
        patch: patch as u16,
    })
}

fn take_digits(b: &[u8], j: &mut usize, min: usize, max: usize) -> Option<u32> {
    let start = *j;
    let mut n: u32 = 0;
    while *j < b.len() && b[*j].is_ascii_digit() && *j - start < max {
        n = n * 10 + (b[*j] - b'0') as u32;
        *j += 1;
    }
    if *j - start < min {
        return None;
    }
    Some(n)
}

pub fn probe() -> Verdict {
    let out = std::process::Command::new(binary())
        .arg("--version")
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            classify(parse_version(&text))
        }
        _ => Verdict::Unknown,
    }
}

// ---------- binary resolution ----------

// Executable file name for this platform.
fn exe_name() -> &'static str {
    if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    }
}

// User data dir: `$XDG_DATA_HOME` or `~/.local/share` (unix),
// `%LOCALAPPDATA%` (windows). Mirrors the `state_dir` logic in `edge.rs`.
fn data_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            return Some(xdg);
        }
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local").join("share"))
    }
}

fn private_path_in(data: &std::path::Path) -> Option<std::path::PathBuf> {
    let p = data.join("rh").join("bin").join(exe_name());
    p.is_file().then_some(p)
}

fn exe_dir_path_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let p = dir.join(exe_name());
    p.is_file().then_some(p)
}

fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        .and_then(|d| exe_dir_path_in(&d))
}

// Resolve the cloudflared binary: private prefix first, then next to the
// `rh` executable, then `PATH` lookup.
pub fn binary() -> std::path::PathBuf {
    if let Some(data) = data_dir()
        && let Some(p) = private_path_in(&data)
    {
        return p;
    }
    if let Some(p) = exe_dir() {
        return p;
    }
    std::path::PathBuf::from("cloudflared")
}

pub fn is_bundled() -> bool {
    if let Some(data) = data_dir()
        && private_path_in(&data).is_some()
    {
        return true;
    }
    exe_dir().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_version_output() {
        assert_eq!(
            parse_version("cloudflared version 2026.9.1 (built 20260912-03:01:36)"),
            Some(CalVer {
                year: 2026,
                minor: 9,
                patch: 1
            })
        );
    }

    #[test]
    fn ignores_build_date_before_version() {
        // The 8-digit build date must not parse as a triple.
        assert_eq!(
            parse_version("built 20260912, cloudflared version 2026.4.0"),
            Some(CalVer {
                year: 2026,
                minor: 4,
                patch: 0
            })
        );
    }

    #[test]
    fn garbage_yields_none_never_old() {
        for s in ["", "version unknown", "v1", "2026.5", "20a6.5.0"] {
            assert_eq!(parse_version(s), None, "input {s:?}");
        }
    }

    #[test]
    fn boundary_classifies_correctly() {
        assert!(matches!(
            classify(parse_version("cloudflared version 2026.5.0")),
            Verdict::Supported(_)
        ));
        assert!(matches!(
            classify(parse_version("cloudflared version 2026.4.9")),
            Verdict::TooOld(_)
        ));
        assert!(matches!(
            classify(parse_version("cloudflared version 2025.7.1")),
            Verdict::TooOld(_)
        ));
        assert!(matches!(classify(None), Verdict::Unknown));
    }

    #[test]
    fn ordering_is_year_then_minor_then_patch() {
        let (a, b, c) = (
            CalVer {
                year: 2026,
                minor: 5,
                patch: 0,
            },
            CalVer {
                year: 2026,
                minor: 4,
                patch: 9,
            },
            CalVer {
                year: 2025,
                minor: 12,
                patch: 0,
            },
        );
        assert!(a > b && b > c);
        assert_eq!(MIN, a);
    }

    #[test]
    fn display_roundtrips() {
        assert_eq!(
            CalVer {
                year: 2026,
                minor: 9,
                patch: 1
            }
            .to_string(),
            "2026.9.1"
        );
    }

    // The pin file is the single source of truth: its MIN must equal the
    // `MIN` const, its PINNED must satisfy MIN, and every platform asset
    // must carry a 64-hex sha256.
    #[test]
    fn pin_file_matches_const() {
        let text =
            std::fs::read_to_string("third-party/cloudflared.pin").expect("pin file must exist");
        let mut min = None;
        let mut pinned = None;
        let mut assets = 0;
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            if let Some(v) = line.strip_prefix("MIN=") {
                min = parse_version(v.trim());
            } else if let Some(v) = line.strip_prefix("PINNED=") {
                pinned = parse_version(v.trim());
            } else {
                let mut parts = line.split_whitespace();
                let (name, hash) = (parts.next(), parts.next());
                if let (Some(n), Some(h)) = (name, hash)
                    && h.len() == 64
                    && h.chars().all(|c| c.is_ascii_hexdigit())
                    && n.starts_with("cloudflared-")
                {
                    assets += 1;
                }
            }
        }
        let min = min.expect("pin file needs a parseable MIN=");
        assert_eq!(min, MIN, "pin MIN diverged from MIN const");
        let pinned = pinned.expect("pin file needs a parseable PINNED=");
        assert!(pinned >= MIN, "pinned {pinned} must satisfy MIN {MIN}");
        assert!(
            assets >= 5,
            "expected linux/mac/windows asset hashes, got {assets}"
        );
        for asset in [
            "cloudflared-linux-amd64",
            "cloudflared-linux-arm64",
            "cloudflared-darwin-amd64.tgz",
            "cloudflared-darwin-arm64.tgz",
            "cloudflared-windows-amd64.exe",
        ] {
            assert!(text.contains(asset), "pin file missing {asset}");
        }
    }

    #[test]
    fn lookup_order_prefers_existing_files() {
        let root = std::env::temp_dir().join(format!("rh-cf-test-{}", std::process::id()));
        let data = root.join("data");
        let exedir = root.join("exedir");
        std::fs::create_dir_all(data.join("rh").join("bin")).unwrap();
        std::fs::create_dir_all(&exedir).unwrap();

        // Nothing exists yet: no resolution.
        assert_eq!(private_path_in(&data), None);
        assert_eq!(exe_dir_path_in(&exedir), None);

        // Private prefix wins when present.
        let priv_bin = data.join("rh").join("bin").join(exe_name());
        std::fs::write(&priv_bin, b"x").unwrap();
        assert_eq!(private_path_in(&data), Some(priv_bin));

        // Exe-dir tier resolves independently.
        let side_bin = exedir.join(exe_name());
        assert_eq!(exe_dir_path_in(&exedir), None);
        std::fs::write(&side_bin, b"x").unwrap();
        assert_eq!(exe_dir_path_in(&exedir), Some(side_bin));

        std::fs::remove_dir_all(&root).ok();
    }
}
