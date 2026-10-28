use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

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

use std::borrow::Cow;

use crate::utils::tty;

// ---------- fetch ----------

const BAR_CELLS: &str = "#..";
const SLOT_BAR_WIDTH: usize = 12;

#[derive(Debug)]
struct FetchFile {
    name: String,
    size: u64,
}

#[derive(Debug)]
struct FetchState {
    slots: Vec<ProgressBar>,
    summary: ProgressBar,
    files: Vec<FetchFile>,
    file_bytes: Vec<AtomicU64>,
    free: Vec<usize>,
    last_summary: Instant,
    speed_at: Instant,
    speed_bytes: u64,
}

#[derive(Debug)]
struct FetchInner {
    mp: MultiProgress,
    slots_n: usize,
    state: Mutex<Option<FetchState>>,
    blocks_done: AtomicU64,
    total_blocks: AtomicU64,
    bytes_done: AtomicU64,
    total_bytes: AtomicU64,
    retries: AtomicU64,
    failed: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct FetchProgress {
    inner: Arc<FetchInner>,
}

impl FetchProgress {
    pub fn new(concurrency: u8) -> Self {
        Self {
            inner: Arc::new(FetchInner {
                mp: MultiProgress::new(),
                slots_n: concurrency.max(1) as usize,
                state: Mutex::new(None),
                blocks_done: AtomicU64::new(0),
                total_blocks: AtomicU64::new(0),
                bytes_done: AtomicU64::new(0),
                total_bytes: AtomicU64::new(0),
                retries: AtomicU64::new(0),
                failed: AtomicU64::new(0),
            }),
        }
    }

    pub fn begin(&self, files: Vec<(String, u64)>, total_blocks: u64, total_bytes: u64) {
        self.inner
            .total_blocks
            .store(total_blocks, Ordering::Relaxed);
        self.inner.total_bytes.store(total_bytes, Ordering::Relaxed);

        let slot_style =
            ProgressStyle::with_template("{msg} [{bar:12}] {percent}% {bytes}/{total_bytes}")
                .expect("fetch slot template")
                .progress_chars(BAR_CELLS);
        debug_assert_eq!(SLOT_BAR_WIDTH, 12);
        let summary_style =
            ProgressStyle::with_template("total [{bar:12}] {pos}/{len} blocks | {msg}")
                .expect("fetch summary template")
                .progress_chars(BAR_CELLS);

        let n = self.inner.slots_n;
        let mut slots = Vec::with_capacity(n);
        for i in 0..n {
            let bar = self.inner.mp.add(ProgressBar::new(1));
            bar.set_style(slot_style.clone());
            bar.set_message(format!("[{}/{}] waiting", i + 1, n));
            slots.push(bar);
        }
        let summary = self.inner.mp.add(ProgressBar::new(1));
        summary.set_style(summary_style);

        let now = Instant::now();
        let file_rows: Vec<FetchFile> = files
            .into_iter()
            .map(|(name, size)| FetchFile { name, size })
            .collect();
        let n_files = file_rows.len();
        let mut guard = self.inner.state.lock().expect("fetch state lock");
        *guard = Some(FetchState {
            slots,
            summary,
            files: file_rows,
            file_bytes: (0..n_files).map(|_| AtomicU64::new(0)).collect(),
            free: (0..n).rev().collect(),
            last_summary: now,
            speed_at: now,
            speed_bytes: 0,
        });
        drop(guard);

        if !crate::utils::tty::is_unicode()
            && let Some(st) = self.inner.state.lock().expect("fetch state lock").as_ref()
        {
            for s in &st.slots {
                s.set_draw_target(indicatif::ProgressDrawTarget::hidden());
            }
            st.summary
                .set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        self.refresh_summary_force();
    }

    pub fn acquire_slot(&self) -> usize {
        if let Some(st) = self.inner.state.lock().expect("fetch state lock").as_mut() {
            st.free.pop().unwrap_or(0)
        } else {
            0
        }
    }

    pub fn release_slot(&self, slot: usize) {
        if let Some(st) = self.inner.state.lock().expect("fetch state lock").as_mut()
            && st.free.len() < st.slots.len()
        {
            st.free.push(slot);
        }
    }

    pub fn file_size(&self, fi: usize) -> u64 {
        self.inner
            .state
            .lock()
            .expect("fetch state lock")
            .as_ref()
            .and_then(|st| st.files.get(fi).map(|f| f.size))
            .unwrap_or(0)
    }

    // clamped to file size; returns new total
    pub fn add_file_bytes(&self, fi: usize, n: u64) -> u64 {
        let guard = self.inner.state.lock().expect("fetch state lock");
        let Some(st) = guard.as_ref() else { return 0 };
        let (Some(row), Some(size)) = (st.file_bytes.get(fi), st.files.get(fi).map(|f| f.size))
        else {
            return 0;
        };
        match row.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some((v + n).min(size))
        }) {
            Ok(prev) | Err(prev) => (prev + n).min(size),
        }
    }

    fn with_slot(&self, slot: usize, f: impl FnOnce(&FetchState, &ProgressBar)) {
        let guard = self.inner.state.lock().expect("fetch state lock");
        if let Some(st) = guard.as_ref()
            && let Some(bar) = st.slots.get(slot)
        {
            f(st, bar);
        }
    }

    fn slot_label(&self, st: &FetchState, slot: usize, fi: usize) -> String {
        let name = st
            .files
            .get(fi)
            .map(|f| truncate_path(&f.name))
            .unwrap_or_else(|| Cow::Owned("?".to_string()));
        format!("[{}/{}] {name}", slot + 1, st.slots.len())
    }

    pub fn slot_begin(&self, slot: usize, fi: usize) {
        self.with_slot(slot, |st, bar| {
            let label = self.slot_label(st, slot, fi);
            let size = st.files.get(fi).map(|f| f.size).unwrap_or(1).max(1);
            let pos = st
                .file_bytes
                .get(fi)
                .map(|b| b.load(Ordering::Relaxed).min(size))
                .unwrap_or(0);
            bar.set_length(size);
            bar.set_position(pos);
            bar.set_message(label);
        });
    }

    pub fn slot_progress(&self, slot: usize, fi: usize) {
        self.with_slot(slot, |st, bar| {
            let size = st.files.get(fi).map(|f| f.size).unwrap_or(1).max(1);
            let pos = st
                .file_bytes
                .get(fi)
                .map(|b| b.load(Ordering::Relaxed).min(size))
                .unwrap_or(0);
            bar.set_position(pos);
        });
    }

    pub fn slot_retry(&self, slot: usize, fi: usize, attempt: u8) {
        self.with_slot(slot, |st, bar| {
            bar.set_message(format!("{} retry {attempt}", self.slot_label(st, slot, fi)));
        });
    }

    pub fn slot_done(&self, slot: usize, fi: usize) {
        self.with_slot(slot, |st, bar| {
            let size = st.files.get(fi).map(|f| f.size).unwrap_or(1).max(1);
            bar.set_position(size);
            bar.set_message(format!("{} done", self.slot_label(st, slot, fi)));
        });
    }

    pub fn slot_failed(&self, slot: usize, hex: &str, err: &str) {
        const KEEP: usize = 8;
        let short = if hex.len() > KEEP { &hex[..KEEP] } else { hex };
        self.with_slot(slot, |st, bar| {
            let n = st.slots.len();
            bar.set_message(format!(
                "[{}/{}] x {short} {err}",
                slot + 1,
                n,
                err = truncate_path(err)
            ));
        });
    }

    pub fn inc_blocks(&self) {
        self.inner.blocks_done.fetch_add(1, Ordering::Relaxed);
        self.refresh_summary();
    }

    pub fn add_bytes(&self, n: u64) {
        self.inner.bytes_done.fetch_add(n, Ordering::Relaxed);
        self.refresh_summary();
    }

    pub fn inc_retries(&self) {
        self.inner.retries.fetch_add(1, Ordering::Relaxed);
        self.refresh_summary();
    }

    pub fn inc_failed(&self) {
        self.inner.failed.fetch_add(1, Ordering::Relaxed);
        self.refresh_summary();
    }

    pub fn retries(&self) -> u64 {
        self.inner.retries.load(Ordering::Relaxed)
    }

    pub fn failed(&self) -> u64 {
        self.inner.failed.load(Ordering::Relaxed)
    }

    pub fn finish_and_clear(&self) {
        if let Some(st) = self.inner.state.lock().expect("fetch state lock").take() {
            for s in &st.slots {
                s.finish_and_clear();
            }
            st.summary.finish_and_clear();
        }
    }

    fn refresh_summary(&self) {
        let mut guard = match self.inner.state.try_lock() {
            Ok(g) => g,
            Err(_) => return, // another task is rendering; skip, don't stall
        };
        let Some(st) = guard.as_mut() else { return };
        if st.last_summary.elapsed() < STATS_MIN_INTERVAL {
            return;
        }
        st.last_summary = Instant::now();
        let tail = self.summary_tail_locked(st);
        let done = self.inner.blocks_done.load(Ordering::Relaxed);
        let total = self.inner.total_blocks.load(Ordering::Relaxed).max(1);
        st.summary.set_length(total);
        st.summary.set_position(done.min(total));
        st.summary.set_message(tail);
    }

    fn refresh_summary_force(&self) {
        let mut guard = self.inner.state.lock().expect("fetch state lock");
        let Some(st) = guard.as_mut() else { return };
        st.last_summary = Instant::now();
        let tail = self.summary_tail_locked(st);
        let done = self.inner.blocks_done.load(Ordering::Relaxed);
        let total = self.inner.total_blocks.load(Ordering::Relaxed).max(1);
        st.summary.set_length(total);
        st.summary.set_position(done.min(total));
        st.summary.set_message(tail);
    }

    fn summary_tail_locked(&self, st: &mut FetchState) -> String {
        let bytes = self.inner.bytes_done.load(Ordering::Relaxed);
        let total_b = self.inner.total_bytes.load(Ordering::Relaxed);
        let retries = self.inner.retries.load(Ordering::Relaxed);
        let failed = self.inner.failed.load(Ordering::Relaxed);

        let now = Instant::now();
        let dt = now.duration_since(st.speed_at).as_secs_f64().max(1e-9);
        let rate = (bytes.saturating_sub(st.speed_bytes)) as f64 / dt;
        st.speed_at = now;
        st.speed_bytes = bytes;

        format!(
            "{}/{} | {}/s | retries {retries} | failed {failed}",
            format_bytes(bytes),
            format_bytes(total_b),
            format_bytes(rate as u64),
        )
    }
}
