use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);
static COLOR: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn init_color(no_color_flag: bool) {
    use std::io::IsTerminal;
    let on = !no_color_flag && std::io::stderr().is_terminal();
    COLOR.store(on, Ordering::Relaxed);
}

pub fn is_color() -> bool {
    COLOR.load(Ordering::Relaxed)
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

// U+2713 CHECK MARK on TTY, plain "ok" otherwise.
pub fn check_ok() -> String {
    if is_color() {
        green("\u{2714}")
    } else {
        "ok".to_string()
    }
}

// U+2717 BALLOT X on TTY, plain "fail" otherwise.
pub fn check_fail() -> String {
    if is_color() {
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
