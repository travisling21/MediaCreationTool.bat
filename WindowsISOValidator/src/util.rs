//! Small shared helpers.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// Progress shared between a worker thread and the UI.
#[derive(Debug)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    pub stage: Mutex<String>,
    pub started: Mutex<Option<Instant>>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            stage: Mutex::new(String::new()),
            started: Mutex::new(None),
        }
    }
}

impl Progress {
    pub fn set_stage(&self, s: impl Into<String>) {
        *self.stage.lock().unwrap() = s.into();
    }
    pub fn stage(&self) -> String {
        self.stage.lock().unwrap().clone()
    }
    pub fn start(&self, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        *self.started.lock().unwrap() = Some(Instant::now());
    }
    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            0.0
        } else {
            (self.done.load(Ordering::Relaxed) as f64 / t as f64).min(1.0) as f32
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    /// Average bytes per second since start.
    pub fn rate(&self) -> f64 {
        let started = self.started.lock().unwrap();
        match *started {
            Some(t) => {
                let secs = t.elapsed().as_secs_f64();
                if secs > 0.2 {
                    self.done.load(Ordering::Relaxed) as f64 / secs
                } else {
                    0.0
                }
            }
            None => 0.0,
        }
    }
}

pub fn human_bytes(n: u64) -> String {
    humansize::format_size(n, humansize::DECIMAL)
}

pub fn human_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 {
        return String::from("-");
    }
    format!("{}/s", humansize::format_size(bytes_per_sec as u64, humansize::DECIMAL))
}

pub fn timestamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// A browser-like user agent. Microsoft's download endpoints reject the reqwest default.
pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/// Normalise a hex digest for comparison.
pub fn norm_hex(s: &str) -> String {
    s.trim().chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_lowercase()
}
