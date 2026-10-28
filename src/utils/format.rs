use std::borrow::Cow;

const MAX_PATH_WIDTH: usize = 60;

pub(crate) fn truncate_path(p: &str) -> Cow<'_, str> {
    if p.len() <= MAX_PATH_WIDTH {
        return Cow::Borrowed(p);
    }
    let tail_len = MAX_PATH_WIDTH - 3;
    let start = p.len() - tail_len;
    let mut i = start;
    while i < p.len() && !p.is_char_boundary(i) {
        i += 1;
    }
    Cow::Owned(format!("...{}", &p[i.min(p.len())..]))
}

pub fn format_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let f = n as f64;
    if f >= GIB {
        format!("{:.1} GiB", f / GIB)
    } else if f >= MIB {
        format!("{:.1} MiB", f / MIB)
    } else if f >= KIB {
        format!("{:.1} KiB", f / KIB)
    } else {
        format!("{n} B")
    }
}
