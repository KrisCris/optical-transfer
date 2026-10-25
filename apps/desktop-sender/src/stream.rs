//! The stream behind the stage: frames made on a worker thread, shown on a
//! flip clock driven by the display.
//!
//! Three rules carried over from the browser sender, where they were measured:
//!
//! * **Encoding never runs on the thread that draws.** A version 40 code takes
//!   a few hundred microseconds to encode and rasterise; nine of them at 60 fps
//!   is a fifth of a core, and on the UI thread that is dropped vsyncs. The
//!   worker runs ahead into a bounded channel and blocks when it is full.
//! * **Every cell that has come due flips on the same display frame.** Waiting
//!   one frame per flip would cap codes × fps at the refresh rate.
//! * **Cells flip staggered.** Cell j flips at phase j/N of the frame interval,
//!   so an exposure straddling a flip loses at most one code, not all of them.

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

/// When each cell flips. Generic over the frame so the timing can be tested
/// without a window or an encoder.
pub struct FlipClock {
    codes: usize,
    interval: Duration,
    sub_interval: Duration,
    next_at: Instant,
    cursor: usize,
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
            codes: 1,
            interval: Duration::ZERO,
            sub_interval: Duration::ZERO,
            next_at: now,
            cursor: 0,
            shown: 0,
            starved: 0,
            flips: VecDeque::new(),
            gaps: VecDeque::new(),
            last_tick: None,
            worst_stall: Duration::ZERO,
        };
        clock.configure(fps, codes);
        clock
    }

    /// Change rate or grid without restarting the stream: the fountain does
    /// not care how its frames are paced, only that they keep coming.
    pub fn configure(&mut self, fps: u32, codes: usize) {
        self.codes = codes.max(1);
        self.interval = Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
        self.sub_interval = self.interval / self.codes as u32;
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
        if let Some(last) = self.last_tick {
            let gap = now.saturating_duration_since(last);
            self.gaps.push_back(gap);
            if self.gaps.len() > 60 {
                self.gaps.pop_front();
            }
            self.worst_stall = self.worst_stall.max(gap);
        }
        self.last_tick = Some(now);
        if now < self.next_at {
            return;
        }
        // More than a whole interval behind means a stall. Resume at rate from
        // here instead of bursting out a backlog no camera saw.
        if now.saturating_duration_since(self.next_at) > self.interval {
            self.next_at = now;
        }
        // Every due cell flips on this frame, but none twice: a second flip
        // before the next vsync paints over a code no camera ever saw, so a
        // rate above the refresh rate runs at the refresh rate, visibly.
        for _ in 0..self.codes {
            if now < self.next_at {
                break;
            }
            let Some(frame) = pull() else {
                self.starved += 1;
                self.next_at = now + self.sub_interval;
                break;
            };
            place(self.cursor, frame);
            self.cursor = (self.cursor + 1) % self.codes;
            self.shown += 1;
            self.flips.push_back(now);
            self.next_at += self.sub_interval;
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

    /// The display's refresh rate, from the median gap between frames.
    pub fn refresh_hz(&self) -> Option<f64> {
        if self.gaps.len() < 10 {
            return None;
        }
        let mut gaps: Vec<_> = self.gaps.iter().copied().collect();
        gaps.sort();
        let median = gaps[gaps.len() / 2].as_secs_f64();
        (median > 0.0).then(|| 1.0 / median)
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
