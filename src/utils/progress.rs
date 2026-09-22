use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

const STATS_MIN_INTERVAL: Duration = Duration::from_millis(100);
const STAGE_MIN_INTERVAL: Duration = Duration::from_millis(50);
const MAX_PATH_WIDTH: usize = 60;

pub enum Phase {
    Scan,
    Check,
    Hash,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Check => "check",
            Self::Hash => "hash",
        }
    }
}

pub enum SkipReason {
    WalkError,
    Symlink,
    NonFile,
    StatFailed,
    BadName,
}

struct Inner {
    _mp: MultiProgress,
    activity: ProgressBar,
    stats: ProgressBar,
    files: AtomicUsize,
    total_files: AtomicUsize,
    has_totals: AtomicUsize, // 0 = unknown, 1 = known
    bytes: AtomicU64,
    total_bytes: AtomicU64,
    blocks_total: AtomicU64,
    blocks_unique: AtomicU64,
    skipped: AtomicUsize,
    last_stats: Mutex<Instant>,
    last_stage: Mutex<(Instant, String)>,
}

#[derive(Clone)]
pub struct IngestProgress {
    inner: Arc<Inner>,
}

impl IngestProgress {
    pub fn new() -> Self {
        let mp = MultiProgress::new();
        let style = ProgressStyle::with_template("{spinner} {msg}")
            .expect("progress template")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);

        let activity = mp.add(ProgressBar::new_spinner());
        activity.set_style(style.clone());
        activity.enable_steady_tick(Duration::from_millis(80));

        let stats = mp.add(ProgressBar::new_spinner());
        stats.set_style(style);
        stats.enable_steady_tick(Duration::from_millis(80));

        let now = Instant::now();
        let this = Self {
            inner: Arc::new(Inner {
                _mp: mp,
                activity,
                stats,
                files: AtomicUsize::new(0),
                total_files: AtomicUsize::new(0),
                has_totals: AtomicUsize::new(0),
                bytes: AtomicU64::new(0),
                total_bytes: AtomicU64::new(0),
                blocks_total: AtomicU64::new(0),
                blocks_unique: AtomicU64::new(0),
                skipped: AtomicUsize::new(0),
                last_stats: Mutex::new(now),
                last_stage: Mutex::new((now, String::new())),
            }),
        };
        this.refresh_stats_force();
        this
    }

    pub fn hidden() -> Self {
        let this = Self::new();
        this.inner
            .activity
            .set_draw_target(indicatif::ProgressDrawTarget::hidden());
        this.inner
            .stats
            .set_draw_target(indicatif::ProgressDrawTarget::hidden());
        this
    }

    pub fn set_stage(&self, phase: Phase, path: &str) {
        let msg = format!("{} : {}", phase.label(), truncate_path(path));
        {
            let mut guard = self.inner.last_stage.lock().expect("stage lock");
            if !guard.1.is_empty() && guard.0.elapsed() < STAGE_MIN_INTERVAL {
                return;
            }
            *guard = (Instant::now(), msg.clone());
        }
        self.inner.activity.set_message(msg);
    }

    pub fn set_totals(&self, files: usize, bytes: u64) {
        self.inner.total_files.store(files, Ordering::Relaxed);
        self.inner.total_bytes.store(bytes, Ordering::Relaxed);
        self.inner.has_totals.store(1, Ordering::Relaxed);
        self.refresh_stats_force();
    }

    pub fn inc_files(&self, n: usize) {
        self.inner.files.fetch_add(n, Ordering::Relaxed);
        self.refresh_stats();
    }

    pub fn add_bytes(&self, n: u64) {
        self.inner.bytes.fetch_add(n, Ordering::Relaxed);
        self.refresh_stats();
    }

    pub fn inc_blocks(&self, unique: bool) {
        self.inner.blocks_total.fetch_add(1, Ordering::Relaxed);
        if unique {
            self.inner.blocks_unique.fetch_add(1, Ordering::Relaxed);
        }
        self.refresh_stats();
    }

    pub fn set_blocks_unique(&self, n: u64) {
        self.inner.blocks_unique.store(n, Ordering::Relaxed);
        self.refresh_stats();
    }

    pub fn inc_skipped(&self, _reason: SkipReason) {
        self.inner.skipped.fetch_add(1, Ordering::Relaxed);
        self.refresh_stats();
    }

    pub fn skipped(&self) -> usize {
        self.inner.skipped.load(Ordering::Relaxed)
    }

    pub fn suspend<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        self.inner.activity.suspend(f)
    }

    pub fn finish_and_clear(&self) {
        self.inner.activity.finish_and_clear();
        self.inner.stats.finish_and_clear();
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
        self.inner.stats.set_message(self.stats_line());
    }

    fn refresh_stats_force(&self) {
        *self.inner.last_stats.lock().expect("stats lock") = Instant::now();
        self.inner.stats.set_message(self.stats_line());
    }

    fn stats_line(&self) -> String {
        let files = self.inner.files.load(Ordering::Relaxed);
        let bytes = self.inner.bytes.load(Ordering::Relaxed);
        let blocks = self.inner.blocks_total.load(Ordering::Relaxed);
        let unique = self.inner.blocks_unique.load(Ordering::Relaxed);
        let skipped = self.inner.skipped.load(Ordering::Relaxed);

        let files_part = if self.inner.has_totals.load(Ordering::Relaxed) == 1 {
            let total = self.inner.total_files.load(Ordering::Relaxed);
            format!("{files}/{total}")
        } else {
            format!("{files}")
        };
        let bytes_part = if self.inner.has_totals.load(Ordering::Relaxed) == 1 {
            let total = self.inner.total_bytes.load(Ordering::Relaxed);
            format!("{}/{}", format_bytes(bytes), format_bytes(total))
        } else {
            format_bytes(bytes)
        };

        format!(
            "files {files_part} | bytes {bytes_part} | blocks {blocks} ({unique} unique) | skipped {skipped}"
        )
    }
}

impl Default for IngestProgress {
    fn default() -> Self {
        Self::new()
    }
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

use std::borrow::Cow;

fn truncate_path(p: &str) -> Cow<'_, str> {
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
        self.refresh_summary_force();
    }

    pub fn hide(&self) {
        if let Some(st) = self.inner.state.lock().expect("fetch state lock").as_ref() {
            for s in &st.slots {
                s.set_draw_target(indicatif::ProgressDrawTarget::hidden());
            }
            st.summary
                .set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
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
        match row.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
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
