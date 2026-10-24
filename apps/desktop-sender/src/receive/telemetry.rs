//! Everything the receive panel and its charts show.
//!
//! A port of the web receiver's `telemetry.js`, so a number in the desktop
//! panel means what the same label means in the browser. Two clocks: pipeline
//! counters (captures, decodes, decoder time) run for the whole camera
//! session; transfer counters restart with each stream and drive speed, ETA
//! and the history chart. Timestamps are milliseconds on the session's clock.

use std::collections::VecDeque;

use optical_core::receiver::ReceiverStats;

const WINDOW_MS: f64 = 2000.0;
pub const BUCKET_MS: f64 = 250.0;
/// One minute of history.
pub const HISTORY_BUCKETS: usize = 240;

#[derive(Debug, Default)]
struct Rolling {
    samples: VecDeque<(f64, f64)>,
}

impl Rolling {
    fn push(&mut self, at: f64, value: f64) {
        self.samples.push_back((at, value));
    }

    /// Sum of values in the last `WINDOW_MS`, pruning older samples.
    fn sum(&mut self, now: f64) -> f64 {
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| *at < now - WINDOW_MS)
        {
            self.samples.pop_front();
        }
        self.samples.iter().map(|(_, value)| value).sum()
    }
}

/// Exponential moving average: the live number should follow the decoder
/// warming up, not the first second's average.
#[derive(Clone, Copy, Debug, Default)]
pub struct Latency {
    count: u64,
    pub recent: f64,
}

impl Latency {
    fn add(&mut self, ms: f64) {
        self.count += 1;
        self.recent = if self.count == 1 {
            ms
        } else {
            self.recent * 0.9 + ms * 0.1
        };
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub captures: u64,
    pub dropped: u64,
    pub full_scans: u64,
    pub crops: u64,
    pub decodes: u64,
    pub symbols: u64,
    pub sightings: u64,
    pub decoder_ms: f64,
}

/// One decode job's outcome, as telemetry needs it.
#[derive(Clone, Copy, Debug)]
pub struct DecodeReport {
    pub full: bool,
    pub symbols: usize,
    pub sightings: usize,
    pub decode_ms: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LatencySnapshot {
    pub full: f64,
    pub crop: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Seconds since the first frame of this stream was captured.
    pub elapsed: f64,
    pub done: bool,
    pub stats: Option<ReceiverStats>,
    pub useful_frames: u64,
    pub live_kibs: f64,
    pub average_kibs: f64,
    pub peak_kibs: f64,
    /// Seconds, while a transfer is running and a rate is known.
    pub eta: Option<f64>,
    pub catch_rate: Option<f64>,
    pub overhead: Option<f64>,
    pub capture_fps: f64,
    pub decode_fps: f64,
    pub symbol_fps: f64,
    /// Share of the pool's wall time spent decoding over the window.
    pub utilization: f64,
    pub latency: LatencySnapshot,
    pub totals: Totals,
}

#[derive(Debug, Default)]
pub struct Telemetry {
    pub workers: usize,
    session_start: Option<f64>,
    captures: Rolling,
    decodes: Rolling,
    symbols: Rolling,
    busy: Rolling,
    totals: Totals,
    full_latency: Latency,
    crop_latency: Latency,
    // Transfer clock.
    start_at: Option<f64>,
    end_at: Option<f64>,
    stats: Option<ReceiverStats>,
    useful_frames: u64,
    useful: Rolling,
    history: Vec<f64>,
    history_start: Option<f64>,
    peak_kibs: f64,
    peak_scanned: usize,
    block_len: Option<usize>,
}

impl Telemetry {
    pub fn new(workers: usize) -> Self {
        Self {
            workers,
            ..Self::default()
        }
    }

    pub fn reset_transfer(&mut self) {
        self.start_at = None;
        self.end_at = None;
        self.stats = None;
        self.useful_frames = 0;
        self.useful = Rolling::default();
        self.history.clear();
        self.history_start = None;
        self.peak_kibs = 0.0;
        self.peak_scanned = 0;
        self.block_len = None;
    }

    // ---------------------------------------------------------------- pipeline

    pub fn capture(&mut self, now: f64, full: bool, crops: usize) {
        self.session_start.get_or_insert(now);
        self.totals.captures += 1;
        self.captures.push(now, 1.0);
        self.totals.full_scans += u64::from(full);
        self.totals.crops += crops as u64;
    }

    pub fn dropped(&mut self, now: f64) {
        self.session_start.get_or_insert(now);
        self.totals.captures += 1;
        self.totals.dropped += 1;
        self.captures.push(now, 1.0);
    }

    pub fn reply(&mut self, now: f64, report: DecodeReport) {
        self.totals.decodes += 1;
        self.decodes.push(now, 1.0);
        self.totals.symbols += report.symbols as u64;
        self.symbols.push(now, report.symbols as f64);
        self.totals.sightings += report.sightings as u64;
        self.totals.decoder_ms += report.decode_ms;
        self.busy.push(now, report.decode_ms);
        if report.full {
            self.full_latency.add(report.decode_ms);
        } else {
            self.crop_latency.add(report.decode_ms);
        }
    }

    // ---------------------------------------------------------------- transfer

    /// A frame of our stream reached the receiver. `captured_at` starts the
    /// transfer clock at the capture that produced the first frame, so aiming
    /// time is not counted. `useful` frames carried new information.
    pub fn accepted(
        &mut self,
        now: f64,
        captured_at: f64,
        useful: bool,
        new_stream: bool,
        block_len: usize,
    ) {
        if new_stream || self.start_at.is_none() {
            self.reset_transfer();
            self.start_at = Some(captured_at);
            self.history_start = Some(captured_at);
        }
        self.block_len = Some(block_len);
        if !useful {
            return;
        }
        self.useful_frames += 1;
        self.useful.push(now, 1.0);
        self.record(now, self.block_len.unwrap_or(0) as f64);
    }

    /// Latest receiver counters.
    pub fn set_stats(&mut self, stats: &ReceiverStats) {
        self.block_len = Some(stats.block_len);
        self.stats = Some(stats.clone());
    }

    pub fn completed(&mut self, now: f64) {
        self.end_at = Some(now);
    }

    pub fn running(&self) -> bool {
        self.start_at.is_some() && self.end_at.is_none()
    }

    fn record(&mut self, now: f64, bytes: f64) {
        let Some(start) = self.history_start else {
            return;
        };
        let bucket = ((now - start) / BUCKET_MS).max(0.0) as usize;
        if self.history.len() <= bucket {
            self.history.resize(bucket + 1, 0.0);
        }
        self.history[bucket] += bytes;
    }

    /// Recovered-data rate history in KiB/s, one point per bucket, newest last.
    pub fn series(&self, now: f64) -> Vec<f64> {
        let Some(start) = self.history_start else {
            return Vec::new();
        };
        let end = ((self.end_at.unwrap_or(now) - start) / BUCKET_MS).max(0.0) as usize;
        let first = (end + 1).saturating_sub(HISTORY_BUCKETS);
        (first..=end)
            .map(|bucket| {
                self.history.get(bucket).copied().unwrap_or(0.0) / 1024.0 * (1000.0 / BUCKET_MS)
            })
            .collect()
    }

    pub fn snapshot(&mut self, now: f64) -> Snapshot {
        let window = self
            .session_start
            .map_or(0.0, |start| (now - start).min(WINDOW_MS));
        let per_second = |rolling: &mut Rolling| {
            if window > 0.0 {
                rolling.sum(now) / (window / 1000.0)
            } else {
                0.0
            }
        };
        let capture_fps = per_second(&mut self.captures);
        let decode_fps = per_second(&mut self.decodes);
        let symbol_fps = per_second(&mut self.symbols);
        let utilization = if window > 0.0 {
            (self.busy.sum(now) / (window * self.workers.max(1) as f64)).min(1.0)
        } else {
            0.0
        };

        let end = self.end_at.unwrap_or(now);
        let elapsed = self
            .start_at
            .map_or(0.0, |start| (end - start).max(0.0) / 1000.0);
        let transfer_window = self
            .start_at
            .map_or(0.0, |start| (now - start).min(WINDOW_MS));
        let useful_rate = if self.end_at.is_none() && transfer_window > 0.0 {
            self.useful.sum(now) / (transfer_window / 1000.0)
        } else {
            0.0
        };
        let block_len = self
            .stats
            .as_ref()
            .map(|stats| stats.block_len)
            .or(self.block_len)
            .unwrap_or(0);
        let live_kibs = useful_rate * block_len as f64 / 1024.0;

        // Peak over a one-second window, from the bucketed history. Windows
        // are scanned once each; the newest bucket is still filling, so it
        // waits.
        let per_window = (1000.0 / BUCKET_MS).round() as usize;
        let settled = if self.end_at.is_none() {
            self.history.len().saturating_sub(1)
        } else {
            self.history.len()
        };
        while self.peak_scanned + per_window <= settled {
            let total: f64 = self.history[self.peak_scanned..self.peak_scanned + per_window]
                .iter()
                .sum();
            self.peak_kibs = self.peak_kibs.max(total / 1024.0);
            self.peak_scanned += 1;
        }

        let stats = self.stats.clone();
        let eta = stats.as_ref().and_then(|stats| {
            // Elimination finishes as soon as there is one useful frame per
            // block, so the floor is k; redundancy beyond that shows up as a
            // lower rate.
            (self.end_at.is_none() && elapsed >= 1.0 && useful_rate > 0.0).then(|| {
                (stats.total_blocks as f64 - self.useful_frames as f64).max(1.0) / useful_rate
            })
        });
        Snapshot {
            elapsed,
            done: self.end_at.is_some(),
            average_kibs: match &stats {
                Some(stats) if elapsed > 0.0 => stats.total_bytes as f64 / 1024.0 / elapsed,
                _ => 0.0,
            },
            catch_rate: stats
                .as_ref()
                .filter(|stats| stats.seq_span > 0)
                .map(|stats| stats.unique_frames as f64 / f64::from(stats.seq_span)),
            overhead: stats
                .as_ref()
                .filter(|stats| stats.total_blocks > 0)
                .map(|stats| stats.unique_frames as f64 / stats.total_blocks as f64),
            stats,
            useful_frames: self.useful_frames,
            live_kibs,
            peak_kibs: self.peak_kibs,
            eta,
            capture_fps,
            decode_fps,
            symbol_fps,
            utilization,
            latency: LatencySnapshot {
                full: self.full_latency.recent,
                crop: self.crop_latency.recent,
            },
            totals: self.totals,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(solved: usize, unique: usize) -> ReceiverStats {
        ReceiverStats {
            unique_frames: unique,
            duplicate_frames: 0,
            redundant_frames: 0,
            solved_blocks: solved,
            total_blocks: 100,
            block_len: 1024,
            total_bytes: 100 * 1024,
            seq_span: 2 * unique as u32,
            pending_equations: 0,
            eliminations: 0,
        }
    }

    #[test]
    fn rates_come_from_useful_frames_and_the_clock_starts_at_capture() {
        let mut telemetry = Telemetry::new(4);
        telemetry.capture(0.0, true, 0);
        for frame in 0..40 {
            let now = 500.0 + f64::from(frame) * 25.0;
            telemetry.capture(now, false, 1);
            telemetry.accepted(now, now - 10.0, true, frame == 0, 1024);
            telemetry.set_stats(&stats(frame as usize, frame as usize + 1));
        }
        let snapshot = telemetry.snapshot(1500.0);
        assert!(
            (snapshot.elapsed - 1.01).abs() < 1e-9,
            "{}",
            snapshot.elapsed
        );
        // 40 useful frames of 1 KiB inside the 1.01 s transfer window.
        assert!(
            (snapshot.live_kibs - 40.0 / 1.01).abs() < 0.5,
            "{}",
            snapshot.live_kibs
        );
        assert_eq!(snapshot.catch_rate, Some(0.5));
        assert_eq!(snapshot.overhead, Some(0.4));
        assert!(snapshot.eta.is_some());
        assert!(snapshot.capture_fps > 20.0);
        assert!(!telemetry.series(1500.0).is_empty());

        telemetry.completed(1600.0);
        let done = telemetry.snapshot(1700.0);
        assert!(done.done);
        assert_eq!(done.eta, None);
        assert_eq!(done.live_kibs, 0.0);
        assert_eq!(done.peak_kibs, 40.0);
    }

    #[test]
    fn decode_latency_is_split_by_path_and_utilisation_is_per_worker() {
        let mut telemetry = Telemetry::new(2);
        telemetry.capture(0.0, true, 1);
        let report = |full, decode_ms| DecodeReport {
            full,
            symbols: 1,
            sightings: 0,
            decode_ms,
        };
        telemetry.reply(100.0, report(true, 20.0));
        telemetry.reply(100.0, report(false, 4.0));
        telemetry.reply(200.0, report(false, 2.0));
        let snapshot = telemetry.snapshot(1000.0);
        assert_eq!(snapshot.latency.full, 20.0);
        assert!((snapshot.latency.crop - 3.8).abs() < 1e-9);
        assert!((snapshot.utilization - 26.0 / 2000.0).abs() < 1e-9);
        assert_eq!(snapshot.totals.symbols, 3);
    }
}
