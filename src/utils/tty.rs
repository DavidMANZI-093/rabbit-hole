use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::task::JoinHandle;

static COLOR: AtomicBool = AtomicBool::new(false);
static UNICODE: AtomicBool = AtomicBool::new(false);
static HEADLINE_WIDTH: AtomicUsize = AtomicUsize::new(0);

// ---------- capability ----------

pub fn init(no_color_flag: bool) {
    let tty = is_tty();
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

pub fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal() && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
}

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

    let color = if no_color_flag {
        false
    } else if matches!(set(env, "FORCE_COLOR"), Some(v) if !v.is_empty() && v != "0" && v != "false")
    {
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

// Insignificant on non-Windows platforms
#[cfg(not(windows))]
fn legacy_console() -> bool {
    false
}

// Returns 'true' when not the modern terminal application 'wt.exe' AND not
// even a popular third-party or integrated environment; boils down to
// the old 'conhost.exe'
#[cfg(windows)]
fn legacy_console() -> bool {
    std::env::var_os("WT_SESSION").is_none() && std::env::var_os("TERM_PROGRAM").is_none()
}

// Unix natively supports ANSI escape sequences
#[cfg(not(windows))]
fn enable_vt() -> bool {
    true
}

// Explicitly enable processing of ANSI esacpe sequences;
// VT_PROCESSING, See https://vt100.net/annarbor/aaa-ug/section13.html)
#[cfg(windows)]
fn enable_vt() -> bool {
    use windows_sys::Win32::{Foundation::INVALID_HANDLE_VALUE, System::Console::*};
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

// ---------- styling ----------

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

pub fn purple(s: &str) -> String {
    if is_color() {
        format!("\x1b[35m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

// ---------- headline ----------

pub(crate) fn draw_headline(line: &str) {
    use std::io::Write;
    clear_headline();
    let _ = write!(std::io::stderr(), "\r{line}");
    let _ = std::io::stderr().flush();
    HEADLINE_WIDTH.store(line.len(), Ordering::Relaxed);
}

pub(crate) fn clear_headline() {
    use std::io::Write;
    let open = HEADLINE_WIDTH.swap(0, Ordering::Relaxed);
    if open > 0 {
        let _ = write!(std::io::stderr(), "\r{}\r", " ".repeat(open));
        let _ = std::io::stderr().flush();
    }
}

// ---------- dot animation ----------

pub const TICK: Duration = Duration::from_millis(300);

const FRAMES: [&str; 3] = [".  ", ".. ", "..."];

struct DotsInner {
    msg: String,
    frame: usize,
    muted: bool,
}

pub struct Dots {
    inner: Arc<Mutex<DotsInner>>,
    ticker: Option<JoinHandle<()>>,
}

impl Dots {
    pub fn start(msg: &str) -> Self {
        Self::start_with(msg, TICK)
    }

    fn start_with(msg: &str, every: Duration) -> Self {
        let muted = !is_tty();
        let inner = Arc::new(Mutex::new(DotsInner {
            msg: msg.to_string(),
            frame: 0,
            muted,
        }));
        let ticker = if muted {
            None
        } else {
            let inner = Arc::clone(&inner);
            Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    advance(&inner);
                }
            }))
        };
        let dots = Self { inner, ticker };
        dots.render();
        dots
    }

    pub fn message(&self, msg: impl Into<String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.msg = msg.into();
        }
        self.render();
    }

    pub fn tick(&self) {
        advance(&self.inner);
    }

    pub fn finish(mut self) {
        self.stop();
    }

    fn render(&self) {
        if let Some((msg, frame)) = snapshot(&self.inner) {
            draw_headline(&format!("{msg}{}", FRAMES[frame]));
        }
    }

    fn stop(&mut self) {
        if let Some(t) = self.ticker.take() {
            t.abort();
        }
        clear_headline();
    }
}

impl Drop for Dots {
    fn drop(&mut self) {
        self.stop();
    }
}

fn snapshot(inner: &Mutex<DotsInner>) -> Option<(String, usize)> {
    let guard = inner.lock().ok()?;
    if guard.muted {
        return None;
    }
    Some((guard.msg.clone(), guard.frame))
}

fn advance(inner: &Arc<Mutex<DotsInner>>) {
    let Ok(mut guard) = inner.lock() else { return };
    if guard.muted {
        return;
    }
    guard.frame = (guard.frame + 1) % FRAMES.len();
    let (msg, frame) = (guard.msg.clone(), guard.frame);
    drop(guard);
    draw_headline(&format!("{msg}{}", FRAMES[frame]));
}

#[cfg(test)]
mod tests {
    use super::resolve;
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
