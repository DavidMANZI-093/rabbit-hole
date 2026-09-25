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
    let out = std::process::Command::new("cloudflared")
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
}
