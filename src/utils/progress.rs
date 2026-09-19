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

fn truncate_path(p: &str) -> String {
    if p.len() <= MAX_PATH_WIDTH {
        return p.to_string();
    }
    let tail_len = MAX_PATH_WIDTH - 3;
    let start = p.len() - tail_len;
    let mut i = start;
    while i < p.len() && !p.is_char_boundary(i) {
        i += 1;
    }
    format!("...{}", &p[i.min(p.len())..])
}
