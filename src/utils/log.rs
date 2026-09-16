use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

// `emit` must be public so macros can expand to it at the call site.
// It is marked `#[doc(hidden)]` to discourage direct use outside of macros.
#[doc(hidden)]
pub fn emit(level: &str, args: std::fmt::Arguments<'_>) {
    eprintln!("{}", format_line(level, args));
}

fn format_line(level: &str, args: std::fmt::Arguments<'_>) -> String {
    format!("({level}) {args}")
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
