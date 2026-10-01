use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);
static COLOR: AtomicBool = AtomicBool::new(false);
static UNICODE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

// Palette invariant: every escape code below is SGR 1, 2, or 30-37
// (basic 16 colors). They render identically on 8-color terminals and
// up, so no color-depth detection exists on purpose. Any future
// 256-color/truecolor code MUST arrive with depth detection alongside it.
pub fn init_color(no_color_flag: bool) {
    use std::io::IsTerminal;
    let tty = std::io::stderr().is_terminal();
    let vt_ok = enable_vt();
    let legacy = legacy_console();
    let env = [
        "NO_COLOR",
        "CLICOLOR",
        "FORCE_COLOR",
        "TERM",
        "WT_SESSION",
        "TERM_PROGRAM",
    ]
    .into_iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
    .collect();
    let (color, unicode) = resolve(no_color_flag, &env, tty, vt_ok, legacy);
    COLOR.store(color, Ordering::Relaxed);
    UNICODE.store(unicode, Ordering::Relaxed);
}

// Capability decision, pure for unit testing (no env access, no syscalls).
// Color precedence, highest first: the `--no-color` flag, then a set
// non-"0" `FORCE_COLOR`, then `NO_COLOR` / `CLICOLOR=0` / `TERM=dumb` /
// non-terminal / missing VT processing on Windows. UNICODE ignores color
// controls entirely: only non-terminals, dumb terms, and legacy consoles
// lose unicode.
fn resolve(
    no_color_flag: bool,
    env: &std::collections::HashMap<String, String>,
    is_tty: bool,
    vt_ok: bool,
    legacy_glyphs: bool,
) -> (bool, bool) {
    fn set<'a>(env: &'a std::collections::HashMap<String, String>, key: &str) -> Option<&'a str> {
        env.get(key).map(String::as_str)
    }
    let forced =
        matches!(set(env, "FORCE_COLOR"), Some(v) if !v.is_empty() && v != "0" && v != "false");
    let color = if no_color_flag {
        false
    } else if forced {
        true
    } else {
        !matches!(set(env, "NO_COLOR"), Some(v) if !v.is_empty())
            && set(env, "CLICOLOR") != Some("0")
            && set(env, "TERM") != Some("dumb")
            && is_tty
            && vt_ok
    };
    let unicode = is_tty && set(env, "TERM") != Some("dumb") && !legacy_glyphs;
    (color, unicode)
}

// Legacy glyph coverage: classic conhost without a modern terminal host.
// Only ever true on Windows (WT_SESSION/TERM_PROGRAM unset); elsewhere false.
#[cfg(not(windows))]
fn legacy_console() -> bool {
    false
}

#[cfg(windows)]
fn legacy_console() -> bool {
    std::env::var_os("WT_SESSION").is_none() && std::env::var_os("TERM_PROGRAM").is_none()
}

// Enable ANSI VT processing for our stderr handle (Windows).
// Succeeds on Win10+ consoles; fails on pipes and pre-Win10 (then
// color stays off via `vt_ok`). No-op elsewhere.
#[cfg(not(windows))]
fn enable_vt() -> bool {
    true
}

#[cfg(windows)]
fn enable_vt() -> bool {
    use windows_sys::Win32::{Foundation::INVALID_HANDLE_VALUE, System::Console::*};
    // SAFETY: plain WinAPI get/set on the process stderr handle. No
    // allocation, no callbacks, no retained state.
    unsafe {
        let h = GetStdHandle(STD_ERROR_HANDLE);
        if h == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut mode: CONSOLE_MODE = 0;
        if GetConsoleMode(h, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

pub fn is_color() -> bool {
    COLOR.load(Ordering::Relaxed)
}

pub fn is_unicode() -> bool {
    UNICODE.load(Ordering::Relaxed)
}

pub fn dim(s: &str) -> String {
    if is_color() {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    if is_color() {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn cyan(s: &str) -> String {
    if is_color() {
        format!("\x1b[34m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn yellow(s: &str) -> String {
    if is_color() {
        format!("\x1b[33m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn green(s: &str) -> String {
    if is_color() {
        format!("\x1b[32m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn red(s: &str) -> String {
    if is_color() {
        format!("\x1b[31m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

// U+2713 CHECK MARK on unicode terminals, plain "ok" otherwise
// (gated on glyphs, not color: NO_COLOR keeps its symbols, uncolored).
pub fn check_ok() -> String {
    if is_unicode() {
        green("\u{2714}")
    } else {
        "ok".to_string()
    }
}

// U+2717 BALLOT X on unicode terminals, plain "fail" otherwise.
pub fn check_fail() -> String {
    if is_unicode() {
        red("\u{2716}")
    } else {
        "fail".to_string()
    }
}

// `emit` must be public so macros can expand to it at the call site.
#[doc(hidden)]
pub fn emit(level: &str, args: std::fmt::Arguments<'_>) {
    match level {
        "warn" => eprintln!("  {}  {args}", yellow("warn")),
        "debug" => eprintln!("  dbg  {args}"),
        _ => eprintln!("  {args}"),
    }
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::utils::log::emit("info", format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::utils::log::emit("warn", format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::utils::log::emit("error", format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::utils::log::is_verbose() {
            $crate::utils::log::emit("debug", format_args!($($arg)*))
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn tty_unix_defaults_to_full() {
        assert_eq!(resolve(false, &env(&[]), true, true, false), (true, true));
    }

    #[test]
    fn flag_kills_color_keeps_unicode() {
        assert_eq!(resolve(true, &env(&[]), true, true, false), (false, true));
    }

    #[test]
    fn no_color_kills_color_keeps_unicode() {
        assert_eq!(
            resolve(false, &env(&[("NO_COLOR", "1")]), true, true, false),
            (false, true)
        );
    }

    #[test]
    fn no_color_empty_is_ignored() {
        assert_eq!(
            resolve(false, &env(&[("NO_COLOR", "")]), true, true, false),
            (true, true)
        );
    }

    #[test]
    fn clicolor_zero_kills_color() {
        assert_eq!(
            resolve(false, &env(&[("CLICOLOR", "0")]), true, true, false),
            (false, true)
        );
    }

    #[test]
    fn force_color_beats_pipe_and_no_color() {
        assert_eq!(
            resolve(
                false,
                &env(&[("FORCE_COLOR", "1"), ("NO_COLOR", "1")]),
                false,
                false,
                false
            ),
            (true, false)
        );
    }

    #[test]
    fn force_color_zero_does_not_force() {
        assert_eq!(
            resolve(false, &env(&[("FORCE_COLOR", "0")]), true, true, false),
            (true, true)
        );
    }

    #[test]
    fn dumb_kills_both() {
        assert_eq!(
            resolve(false, &env(&[("TERM", "dumb")]), true, true, false),
            (false, false)
        );
    }

    #[test]
    fn piped_kills_both() {
        assert_eq!(
            resolve(false, &env(&[]), false, true, false),
            (false, false)
        );
    }

    #[test]
    fn legacy_kills_unicode_keeps_color_when_vt_ok() {
        assert_eq!(resolve(false, &env(&[]), true, true, true), (true, false));
    }

    #[test]
    fn legacy_without_vt_kills_color_too() {
        assert_eq!(resolve(false, &env(&[]), true, false, true), (false, false));
    }
}
