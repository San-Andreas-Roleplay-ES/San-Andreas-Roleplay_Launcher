//! Progress payload shared by the GTA and cache installers (`GtaInstallProgress`
//! / `CacheInstallProgress` in the frontend), plus the download speed meter.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

const SPEED_WINDOW: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Preflight,
    Download,
    Verify,
    Extract,
    Shortcut,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: Phase,
    pub percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_done: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_bps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Progress {
    pub fn new(phase: Phase, percent: f64) -> Self {
        Self {
            phase,
            percent,
            bytes_done: None,
            bytes_total: None,
            speed_bps: None,
            message: None,
        }
    }

    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn bytes(mut self, done: u64, total: u64) -> Self {
        self.bytes_done = Some(done);
        self.bytes_total = Some(total);
        self
    }
}

/// Progress sink. `Arc` so it can cross into blocking threads (zip extraction).
pub type Reporter = Arc<dyn Fn(Progress) + Send + Sync>;

/// `done / total` as a 0–100 percentage, capped at 100. Zero when the total is unknown.
pub fn percent(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64 * 100.0).min(100.0)
    }
}

/// Throughput over a sliding window, so the number reacts to speed changes.
#[derive(Default)]
pub struct SpeedMeter {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedMeter {
    pub fn push(&mut self, now: Instant, bytes: u64) {
        self.samples.push_back((now, bytes));
        while let Some(&(t, _)) = self.samples.front() {
            if now.duration_since(t) > SPEED_WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn bytes_per_second(&self, now: Instant) -> u64 {
        let Some(&(first, _)) = self.samples.front() else {
            return 0;
        };
        let bytes: u64 = self.samples.iter().map(|(_, b)| b).sum();
        let ms = now.duration_since(first).as_millis().max(1) as u64;
        (bytes as f64 * 1000.0 / ms as f64).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_like_the_ts_interface() {
        let full = Progress::new(Phase::Download, 12.5)
            .bytes(10, 80)
            .message("hola");
        let full = Progress {
            speed_bps: Some(99),
            ..full
        };
        assert_eq!(
            serde_json::to_value(&full).unwrap(),
            json!({
                "phase": "download",
                "percent": 12.5,
                "bytesDone": 10,
                "bytesTotal": 80,
                "speedBps": 99,
                "message": "hola"
            })
        );
        // Optional fields are omitted, not null.
        assert_eq!(
            serde_json::to_value(Progress::new(Phase::Done, 100.0)).unwrap(),
            json!({ "phase": "done", "percent": 100.0 })
        );
    }

    #[test]
    fn percent_is_capped_and_safe() {
        assert_eq!(percent(50, 200), 25.0);
        assert_eq!(percent(300, 200), 100.0);
        assert_eq!(percent(5, 0), 0.0);
    }

    #[test]
    fn speed_meter_uses_a_sliding_window() {
        let t0 = Instant::now();
        let mut meter = SpeedMeter::default();
        meter.push(t0, 1000);
        meter.push(t0 + Duration::from_millis(500), 1000);
        assert_eq!(
            meter.bytes_per_second(t0 + Duration::from_millis(500)),
            4000
        );
        // Samples older than the window drop out.
        meter.push(t0 + Duration::from_secs(5), 500);
        assert_eq!(meter.samples.len(), 1);
    }
}
