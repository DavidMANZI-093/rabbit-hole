// Copyright (c) 2026 David MANZI
// SPDX-License-Identifier: MIT

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::utils::format::{format_bytes, truncate_path};

// ---------- Serve/Ingest ----------

const STATS_MIN_INTERVAL: Duration = Duration::from_millis(100);

struct Inner {
    source: String,
    muted: bool,
    files: AtomicUsize,
    total_files: AtomicUsize,
    has_totals: AtomicUsize, // 0 = unknown, 1 = known
    bytes: AtomicU64,
    total_bytes: AtomicU64,
    last_stats: Mutex<Instant>,
}

#[derive(Clone)]
pub struct IngestProgress {
    inner: Arc<Inner>,
}

impl Default for IngestProgress {
    fn default() -> Self {
        Self::new(std::path::Path::new(""))
    }
}

impl IngestProgress {
    pub fn new(source: &std::path::Path) -> Self {
        Self::build(
            truncate_path(&source.display().to_string()).into_owned(),
            !crate::utils::tty::is_tty(),
        )
    }

    fn build(source: String, muted: bool) -> Self {
        let this = Self {
            inner: Arc::new(Inner {
                source,
                muted,
                files: AtomicUsize::new(0),
                total_files: AtomicUsize::new(0),
                has_totals: AtomicUsize::new(0),
                bytes: AtomicU64::new(0),
                total_bytes: AtomicU64::new(0),
                last_stats: Mutex::new(Instant::now()),
            }),
        };
        this.refresh_stats_force();

        this
    }

    // Only for tests (no need for tty).
    pub fn _new() -> Self {
        Self::build(String::new(), true)
    }

    pub fn set_totals(&self, files: usize, bytes: u64) {
        self.inner.total_files.store(files, Ordering::Relaxed);
        self.inner.total_bytes.store(bytes, Ordering::Relaxed);
        self.inner.has_totals.store(1, Ordering::Relaxed);
        self.refresh_stats_force();
    }

    pub fn add_files(&self, n: usize) {
        self.inner.files.fetch_add(n, Ordering::Relaxed);
        self.refresh_stats();
    }

    pub fn add_bytes(&self, n: u64) {
        self.inner.bytes.fetch_add(n, Ordering::Relaxed);
        self.refresh_stats();
    }

    pub fn finish_and_clear(&self) {
        tty::clear_headline();
    }

    fn refresh_stats(&self) {
        let mut guard = match self.inner.last_stats.try_lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.elapsed() < STATS_MIN_INTERVAL {
            return;
        }
        *guard = Instant::now();
        drop(guard);
        self.draw();
    }

    // Can be called when 'IngestProgress' is surely lock-free
    fn refresh_stats_force(&self) {
        *self
            .inner
            .last_stats
            .lock()
            .expect("failed to acquire lock on 'IngestProgress'") = Instant::now();
        self.draw();
    }

    fn draw(&self) {
        if self.inner.muted {
            return;
        }
        tty::draw_headline(&self.headline());
    }

    fn headline(&self) -> String {
        let files = self.inner.files.load(Ordering::Relaxed);
        let bytes = self.inner.bytes.load(Ordering::Relaxed);
        let (total_files, total_bytes) = if self.inner.has_totals.load(Ordering::Relaxed) == 1 {
            (
                Some(self.inner.total_files.load(Ordering::Relaxed)),
                Some(self.inner.total_bytes.load(Ordering::Relaxed)),
            )
        } else {
            (None, None)
        };
        compose_line(&self.inner.source, files, total_files, bytes, total_bytes)
    }
}

fn compose_line(
    source: &str,
    files: usize,
    total_files: Option<usize>,
    bytes: u64,
    total_bytes: Option<u64>,
) -> String {
    let files_part = match total_files {
        Some(t) => format!("{files}/{t}"),
        None => format!("{files}"),
    };
    let bytes_part = match total_bytes {
        Some(t) => format!("{}/{}", format_bytes(bytes), format_bytes(t)),
        None => format_bytes(bytes),
    };

    format!("ingesting {source} ({files_part} files, {bytes_part})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headline_empty_unknown_totals() {
        assert_eq!(
            compose_line("src", 0, None, 0, None),
            "ingesting src (0 files, 0 B)"
        );
    }

    #[test]
    fn headline_known_totals() {
        assert_eq!(
            compose_line("photos", 412, Some(3240), 419430400, Some(3543348019)),
            "ingesting photos (412/3240 files, 400.0 MiB/3.3 GiB)"
        );
    }

    #[test]
    fn headline_grows_with_counters() {
        let before = compose_line("src", 3, None, 300, None);
        let after = compose_line("src", 41, None, 5200, None);
        assert!(after.len() > before.len());
        assert!(after.contains("(41 files, "));
    }

    #[test]
    fn headline_clear_is_noop_when_nothing_open() {
        use crate::utils::tty::clear_headline;
        clear_headline();
        clear_headline();
    }
}

use crate::utils::tty;

// ---------- fetch ----------

const RATE_WINDOW: Duration = Duration::from_secs(1);

struct Render {
    last_draw: Instant,
    window_at: Instant,
    window_bytes: u64,
    rate: u64,
}

struct FetchInner {
    started: AtomicBool,
    files: AtomicUsize,
    blocks_done: AtomicU64,
    total_blocks: AtomicU64,
    bytes_done: AtomicU64,
    total_bytes: AtomicU64,
    retries: AtomicU64,
    failed: AtomicU64,
    muted: bool,
    render: Mutex<Render>,
}

#[derive(Clone)]
pub struct FetchProgress {
    inner: Arc<FetchInner>,
}

#[derive(Debug, Clone, Copy)]
pub struct FetchSnapshot {
    pub started: bool,
    pub files: usize,
    pub blocks_done: u64,
    pub blocks_total: u64,
    pub bytes: u64,
    pub total_bytes: u64,
    pub retries: u64,
    pub failed: u64,
}

impl Default for FetchProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl FetchProgress {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            inner: Arc::new(FetchInner {
                started: AtomicBool::new(false),
                files: AtomicUsize::new(0),
                blocks_done: AtomicU64::new(0),
                total_blocks: AtomicU64::new(0),
                bytes_done: AtomicU64::new(0),
                total_bytes: AtomicU64::new(0),
                retries: AtomicU64::new(0),
                failed: AtomicU64::new(0),
                muted: !tty::is_tty(),
                render: Mutex::new(Render {
                    last_draw: now,
                    window_at: now,
                    window_bytes: 0,
                    rate: 0,
                }),
            }),
        }
    }

    pub fn begin(&self, files: usize, total_blocks: u64, total_bytes: u64) {
        self.inner.files.store(files, Ordering::Relaxed);
        self.inner.blocks_done.store(0, Ordering::Relaxed);
        self.inner
            .total_blocks
            .store(total_blocks, Ordering::Relaxed);
        self.inner.bytes_done.store(0, Ordering::Relaxed);
        self.inner.total_bytes.store(total_bytes, Ordering::Relaxed);
        self.inner.retries.store(0, Ordering::Relaxed);
        self.inner.failed.store(0, Ordering::Relaxed);
        self.inner.started.store(true, Ordering::Relaxed);

        if let Ok(mut r) = self.inner.render.lock() {
            r.window_at = Instant::now();
            r.window_bytes = 0;
            r.rate = 0;
        }
        self.refresh(true);
    }

    pub fn add_bytes(&self, n: u64) {
        self.inner.bytes_done.fetch_add(n, Ordering::Relaxed);
        self.refresh(false);
    }

    pub fn inc_blocks(&self) {
        self.inner.blocks_done.fetch_add(1, Ordering::Relaxed);
        self.refresh(false);
    }

    pub fn inc_retries(&self) {
        self.inner.retries.fetch_add(1, Ordering::Relaxed);
        self.refresh(false);
    }

    pub fn inc_failed(&self) {
        self.inner.failed.fetch_add(1, Ordering::Relaxed);
        self.refresh(false);
    }

    pub fn snapshot(&self) -> FetchSnapshot {
        FetchSnapshot {
            started: self.inner.started.load(Ordering::Relaxed),
            files: self.inner.files.load(Ordering::Relaxed),
            blocks_done: self.inner.blocks_done.load(Ordering::Relaxed),
            blocks_total: self.inner.total_blocks.load(Ordering::Relaxed),
            bytes: self.inner.bytes_done.load(Ordering::Relaxed),
            total_bytes: self.inner.total_bytes.load(Ordering::Relaxed),
            retries: self.inner.retries.load(Ordering::Relaxed),
            failed: self.inner.failed.load(Ordering::Relaxed),
        }
    }

    pub fn finish_and_clear(&self) {
        tty::clear_headline();
    }

    fn refresh(&self, force: bool) {
        if self.inner.muted {
            return;
        }
        let Ok(mut r) = self.inner.render.try_lock() else {
            return;
        };
        let now = Instant::now();
        if !force && now.duration_since(r.last_draw) < STATS_MIN_INTERVAL {
            return;
        }
        r.last_draw = now;

        let bytes = self.inner.bytes_done.load(Ordering::Relaxed);
        let dt = now.duration_since(r.window_at);
        // A 1s window is stable but means short transfers never show a rate;
        // take the first estimate as soon as there is a measurable delta.
        if dt >= RATE_WINDOW || (r.rate == 0 && dt >= STATS_MIN_INTERVAL) {
            let secs = dt.as_secs_f64().max(1e-9);
            r.rate = (bytes.saturating_sub(r.window_bytes) as f64 / secs) as u64;
            r.window_at = now;
            r.window_bytes = bytes;
        }
        let rate = r.rate;
        drop(r);

        let line = fetch_line(
            self.inner.blocks_done.load(Ordering::Relaxed),
            self.inner.total_blocks.load(Ordering::Relaxed),
            rate,
            self.inner.retries.load(Ordering::Relaxed),
            self.inner.failed.load(Ordering::Relaxed),
        );
        tty::draw_headline(&line);
    }
}

fn fetch_line(blocks_done: u64, blocks_total: u64, rate: u64, retries: u64, failed: u64) -> String {
    let mut line = if blocks_total == 0 {
        "fetching 0/0 blocks".to_string()
    } else {
        // Truncated, not rounded: 3/17 reads as 17.64%, not 17.65%.
        let pct = blocks_done
            .saturating_mul(10_000)
            .checked_div(blocks_total)
            .map(|q| q as f64 / 100.0)
            .unwrap_or(0.0);
        format!("fetching {pct:.2}% ({blocks_done}/{blocks_total} blocks)")
    };
    line.push_str(&format!(", {}/s", format_bytes(rate)));
    if retries > 0 {
        line.push_str(&format!(", {retries} retries"));
    }
    if failed > 0 {
        line.push_str(&format!(", {failed} failed"));
    }
    line
}

#[cfg(test)]
mod fetch_tests {
    use super::*;

    #[test]
    fn line_reports_percent_and_rate() {
        assert_eq!(
            fetch_line(3, 17, 1_468_006, 0, 0),
            "fetching 17.64% (3/17 blocks), 1.4 MiB/s"
        );
    }

    #[test]
    fn line_appends_retries_and_failures() {
        assert_eq!(
            fetch_line(0, 0, 0, 2, 1),
            "fetching 0/0 blocks, 0 B/s, 2 retries, 1 failed"
        );
    }

    #[test]
    fn line_reaches_full_percent() {
        assert_eq!(
            fetch_line(17, 17, 0, 0, 0),
            "fetching 100.00% (17/17 blocks), 0 B/s"
        );
    }

    #[test]
    fn snapshot_reflects_counters() {
        let p = FetchProgress::new();
        p.begin(2, 4, 100);
        p.add_bytes(50);
        p.inc_blocks();
        p.inc_retries();
        p.inc_failed();

        let s = p.snapshot();
        assert!(s.started);
        assert_eq!(s.files, 2);
        assert_eq!(s.blocks_done, 1);
        assert_eq!(s.blocks_total, 4);
        assert_eq!(s.bytes, 50);
        assert_eq!(s.total_bytes, 100);
        assert_eq!(s.retries, 1);
        assert_eq!(s.failed, 1);
    }
}
