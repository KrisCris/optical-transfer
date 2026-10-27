//! The stream behind the stage: frames made on a worker thread, shown on a
//! flip clock driven by the display.
//!
//! Four rules carried over from the browser sender, where they were measured:
//!
//! * **Encoding never runs on the thread that draws.** A version 40 code takes
//!   a few hundred microseconds to encode and rasterise; nine of them at 60 fps
//!   is a fifth of a core, and on the UI thread that is dropped vsyncs. The
//!   worker runs ahead into a bounded channel and blocks when it is full.
//! * **Every code stays up whole display frames.** A phone camera reads a code
//!   only while it holds still, so each stays up the fewest whole refreshes not
//!   much shorter than `1 / fps` ([`hold_frames`]); fps is a ceiling.
//! * **Every cell that has come due flips on the same display frame.** Waiting
//!   one frame per flip would cap codes × fps at the refresh rate.
//! * **Cells flip staggered.** Cell j flips j/N of the way through the hold,
//!   so an exposure straddling a flip loses only the codes changing on that
//!   frame, not all of them.

use std::{
    collections::VecDeque,
    sync::Arc,
    sync::mpsc::{Receiver, TryRecvError, sync_channel},
    thread,
    time::{Duration, Instant},
};

use eframe::egui::{Color32, ColorImage, vec2};
use optical_core::{
    container::Compression,
    qr::compose,
    transfer::{Transfer, TransferSettings},
};

/// Frames the worker may run ahead. Enough for three full nine-code grids.
const AHEAD: usize = 27;

/// A rendered code, already in the texture's pixel format: one frame, or
/// three in a colour code.
pub struct Frame {
    pub image: Arc<ColorImage>,
    /// Modules per side, quiet zone included.
    pub size: usize,
    pub version: u8,
}

/// What the stage and the stats need to know about a stream, captured before
/// the transfer moves to its worker.
#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub name: String,
    pub settings: TransferSettings,
    /// v3's session id or v4's container checksum: what a receiver tells
    /// streams apart by.
    pub stream_id: u32,
    /// Frames one clean systematic sweep takes: v3 blocks, v4 symbols.
    pub source_blocks: u32,
    pub block_len: usize,
    /// v3's carousel length; `None` on v4, whose repair symbols never repeat.
    pub cycle_frames: Option<u32>,
    pub compression: Compression,
    pub original_size: usize,
    pub transmitted_size: usize,
}

/// The frame worker. Dropping this stops it: its next send fails and it exits.
pub struct FrameSource {
    frames: Receiver<Result<Frame, String>>,
    pub info: StreamInfo,
}

impl FrameSource {
    pub fn spawn(mut transfer: Transfer) -> Self {
        let info = StreamInfo {
            name: transfer.name().to_owned(),
            settings: transfer.settings(),
            stream_id: transfer.stream_id(),
            source_blocks: transfer.source_blocks(),
            block_len: transfer.block_len(),
            cycle_frames: transfer.cycle_frames(),
            compression: transfer.compression(),
            original_size: transfer.original_size(),
            transmitted_size: transfer.transmitted_size(),
        };
        let (sender, frames) = sync_channel(AHEAD);
        thread::spawn(move || {
            let mut renderer = transfer.renderer();
            let layers = transfer.settings().layers();
            loop {
                let frame = (0..layers)
                    .map(|_| renderer.render(&transfer.next_payload()))
                    .collect::<Result<Vec<_>, _>>()
                    .map(|codes| Frame {
                        image: Arc::new(to_image(codes[0].size, &compose(&codes))),
                        size: codes[0].size,
                        version: codes[0].version,
                    })
                    .map_err(|error| format!("This frame could not be drawn: {error}."));
                let failed = frame.is_err();
                if sender.send(frame).is_err() || failed {
                    return;
                }
            }
        });
        Self { frames, info }
    }

    /// The next frame if the worker has one ready. `Err` is final.
    pub fn try_next(&self) -> Option<Result<Frame, String>> {
        match self.frames.try_recv() {
            Ok(frame) => Some(frame),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("The frame generator stopped unexpectedly.".to_owned()))
            }
        }
    }
}

/// One pixel per module, each channel black where its layer is dark (see
/// [`compose`]): pure black on pure white for one layer, a colour code for
/// three. Scaling is the texture's job, and it is nearest-neighbour at an
/// integer factor.
fn to_image(size: usize, modules: &[u8]) -> ColorImage {
    let level = |dark: bool| if dark { 0 } else { 255 };
    ColorImage {
        size: [size, size],
        pixels: modules
            .iter()
            .map(|&bits| {
                Color32::from_rgb(
                    level(bits & 1 != 0),
                    level(bits & 2 != 0),
                    level(bits & 4 != 0),
                )
            })
            .collect(),
        source_size: vec2(size as f32, size as f32),
    }
}

/// How much shorter than `1 / fps` a code may stay up, so that a refresh
/// measured a hair long does not add a whole frame to every code.
const HOLD_SLACK: f64 = 0.95;

/// Frame spacing assumed until a few frames have been measured.
const ASSUMED_PERIOD: Duration = Duration::from_nanos(16_666_667);

/// Codes up for less than this lose most of themselves to a 60 fps camera.
pub const SHORT_HOLD: Duration = Duration::from_millis(25);

/// Display frames a code stays up at `fps` on a display that shows a frame
/// every `period`: the fewest whole frames not much shorter than `1 / fps`.
///
/// A phone filming at 60 fps reads a code only while it holds still for most
/// of two of its frames. 60 fps on a 144 Hz display held codes three and two
/// refreshes in turn (21 and 14 ms), and an iPhone read the 14 ms ones about
/// one time in eight against two in three for the rest. Here 60 fps at 144 Hz
/// is three refreshes each, 48 codes a second, and 36 fps four.
pub fn hold_frames(fps: u32, period: Duration) -> u32 {
    let period = period.as_secs_f64();
    if period <= 0.0 {
        return 1;
    }
    ((HOLD_SLACK / (f64::from(fps.max(1)) * period)).ceil() as u32).max(1)
}

/// When each cell flips. Generic over the frame so the timing can be tested
/// without a window or an encoder.
///
/// Time is counted in display frames, measured from the spacing of
/// [`FlipClock::tick`] calls, so every code stays up a whole number of them
/// and a frame the window skips lengthens a code rather than shortening the
/// next.
pub struct FlipClock {
    codes: usize,
    fps: u32,
    cursor: usize,
    /// Display frames since the stream started, skipped ones included.
    refreshes: u64,
    /// The display frame the next cell is due on: fractional, so the cells
    /// spread evenly through a hold.
    due_at: f64,
    /// The display frame each cell's code went up on.
    up_since: Vec<Option<u64>>,
    /// Codes shown since the stream started. Times the frames in each code,
    /// that is also the next frame's sequence number: the worker produces
    /// them in order from zero.
    pub shown: u64,
    /// Times a frame was due and the worker had none ready.
    pub starved: u64,
    flips: VecDeque<Instant>,
    gaps: VecDeque<Duration>,
    last_tick: Option<Instant>,
    /// Longest gap between display frames. Over a second means the window was
    /// hidden or throttled, which freezes the stream mid-flip.
    pub worst_stall: Duration,
}

impl FlipClock {
    pub fn new(fps: u32, codes: usize, now: Instant) -> Self {
        let mut clock = Self {
            codes: 0,
            fps: 1,
            cursor: 0,
            refreshes: 0,
            due_at: 1.0,
            up_since: Vec::new(),
            shown: 0,
            starved: 0,
            flips: VecDeque::new(),
            gaps: VecDeque::new(),
            last_tick: Some(now),
            worst_stall: Duration::ZERO,
        };
        clock.configure(fps, codes);
        clock
    }

    /// Change rate or grid without restarting the stream: the fountain does
    /// not care how its frames are paced, only that they keep coming.
    pub fn configure(&mut self, fps: u32, codes: usize) {
        let codes = codes.max(1);
        if codes != self.codes {
            self.up_since = vec![None; codes];
        }
        self.codes = codes;
        self.fps = fps.max(1);
        self.cursor %= self.codes;
    }

    pub fn codes(&self) -> usize {
        self.codes
    }

    /// Run once per display frame. Pulls a frame for every flip that is due
    /// and hands each to `place` with the cell it belongs in.
    pub fn tick<T>(
        &mut self,
        now: Instant,
        mut pull: impl FnMut() -> Option<T>,
        mut place: impl FnMut(usize, T),
    ) {
        let gap = self
            .last_tick
            .map(|last| now.saturating_duration_since(last));
        self.last_tick = Some(now);
        if let Some(gap) = gap {
            self.gaps.push_back(gap);
            if self.gaps.len() > 60 {
                self.gaps.pop_front();
            }
            self.worst_stall = self.worst_stall.max(gap);
        }
        // A gap of several frames is frames the window skipped: they count.
        let period = self.period();
        self.refreshes += match (gap, period) {
            (Some(gap), Some(period)) => {
                ((gap.as_secs_f64() / period.as_secs_f64()).round() as u64).max(1)
            }
            _ => 1,
        };
        let hold = hold_frames(self.fps, period.unwrap_or(ASSUMED_PERIOD));
        let at = self.refreshes as f64;
        // Every due cell flips on this frame, but none twice: a second flip
        // before the next vsync paints over a code no camera ever saw, so a
        // rate above the refresh rate runs at the refresh rate, visibly.
        for _ in 0..self.codes {
            if at < self.due_at - 0.5 {
                break;
            }
            // A code stays up its whole hold, even when the schedule runs late.
            if self.up_since[self.cursor]
                .is_some_and(|since| self.refreshes - since < u64::from(hold))
            {
                break;
            }
            let Some(frame) = pull() else {
                self.starved += 1;
                break;
            };
            place(self.cursor, frame);
            self.up_since[self.cursor] = Some(self.refreshes);
            self.cursor = (self.cursor + 1) % self.codes;
            self.shown += 1;
            self.flips.push_back(now);
            // A late flip (a stall, an empty queue) moves the schedule with
            // it, so the stream resumes at rate instead of bursting out a
            // backlog no camera saw.
            if at - self.due_at >= 0.5 {
                self.due_at = at;
            }
            self.due_at += f64::from(hold) / self.codes as f64;
        }
        while self
            .flips
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > Duration::from_secs(1))
        {
            self.flips.pop_front();
        }
    }

    /// Flips in the last second.
    pub fn flip_rate(&self) -> usize {
        self.flips.len()
    }

    /// Time between display frames: the median gap between ticks, which an
    /// occasional skipped frame does not move. `None` for the first few
    /// frames, which count one each.
    pub fn period(&self) -> Option<Duration> {
        if self.gaps.len() < 3 {
            return None;
        }
        let mut gaps: Vec<_> = self.gaps.iter().copied().collect();
        gaps.sort();
        let median = gaps[gaps.len() / 2];
        (!median.is_zero()).then_some(median)
    }

    /// The display's refresh rate, from the median gap between frames.
    pub fn refresh_hz(&self) -> Option<f64> {
        if self.gaps.len() < 10 {
            return None;
        }
        self.period().map(|period| 1.0 / period.as_secs_f64())
    }

    /// How long each code stays up on this display: whole frames, and the
    /// time they come to.
    pub fn hold(&self) -> (u32, Duration) {
        let period = self.period().unwrap_or(ASSUMED_PERIOD);
        let frames = hold_frames(self.fps, period);
        (frames, period * frames)
    }
}

/// The `cols × rows` tiling of `codes` square cells of `cell` modules that
/// gives each module the most room in `width × height`: side by side on a wide
/// window, stacked on a tall one.
pub fn best_grid(codes: usize, cell: usize, width: f32, height: f32) -> (usize, usize) {
    let codes = codes.max(1);
    let cell = cell.max(1) as f32;
    let mut best = (1, codes, f32::MIN);
    for cols in (1..=codes).filter(|cols| codes.is_multiple_of(*cols)) {
        let rows = codes / cols;
        let fit = (width / (cols as f32 * cell)).min(height / (rows as f32 * cell));
        if fit > best.2 {
            best = (cols, rows, fit);
        }
    }
    (best.0, best.1)
}
