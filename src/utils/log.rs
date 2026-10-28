use std::sync::atomic::{AtomicBool, Ordering};

use crate::utils::tty::{bold, clear_headline, purple, red, yellow};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

// `emit` must be public so macros can expand to it at the call site.
#[doc(hidden)]
pub fn emit(level: &str, msg: String) {
    clear_headline();
    match level {
        "warn" => eprintln!("{}: {msg}", bold(yellow("warning").as_str())),
        "debug" => eprintln!("{}: {msg}", bold(purple("debug").as_str())),
        "error" => eprintln!("{}: {msg}", bold(red("error").as_str())),
        "info" => eprintln!("{msg}"),
        unk => unreachable!("emit with unknown log level '{unk}'"),
    }
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::utils::log::emit("info", ::std::format!($($arg)*))
    };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::utils::log::emit("warn", ::std::format!($($arg)*))
    };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::utils::log::emit("error", ::std::format!($($arg)*))
    };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::utils::log::is_verbose() {
            $crate::utils::log::emit("debug", ::std::format!($($arg)*))
        }
    };
}
