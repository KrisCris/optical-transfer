//! A camera that films a sender which exists only in memory.
//!
//! For tests and the `receive_demo` example: frames `optical-core` renders,
//! drawn the way a camera sees a screen — fractional module pitch, a slight
//! rotation, a hand that drifts, the blur of averaging several samples per
//! pixel — so everything after the camera runs exactly as it does on real
//! captures. What it does not model is a real lens or a real display, which
//! is what the end-to-end runs in `tests/end-to-end` are for.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use optical_core::{qr::QrFrame, transfer::Transfer};

use crate::receive::camera::{FrameSink, LumaFrame, Negotiated};

const DARK: f32 = 40.0;
const LIGHT: f32 = 215.0;

/// Where and how the codes sit in the picture.
#[derive(Clone, Copy, Debug)]
pub struct Framing {
    pub width: usize,
    pub height: usize,
    /// Codes per row of the sender's grid.
    pub columns: usize,
    /// Camera pixels per QR module.
    pub pitch: f32,
    pub degrees: f32,
    /// Shift of the whole grid, in camera pixels.
    pub offset: (f32, f32),
}

/// Draw `codes` into a luma frame, each centred in its grid cell.
pub fn camera_frame(codes: &[QrFrame], framing: Framing) -> LumaFrame {
    let Framing {
        width,
        height,
        columns,
        pitch,
        degrees,
        offset,
    } = framing;
    let columns = columns.max(1);
    let rows = codes.len().div_ceil(columns).max(1);
    let (cell_w, cell_h) = (width / columns, height / rows);
    let (sin, cos) = degrees.to_radians().sin_cos();
    // The luma of one sample point, in whichever code's cell it falls.
    let sample = |x: f32, y: f32| {
        let (column, row) = ((x as usize) / cell_w.max(1), (y as usize) / cell_h.max(1));
        let Some(code) = codes
            .get(row * columns + column)
            .filter(|_| column < columns)
        else {
            return LIGHT;
        };
        let center = (
            (column * cell_w) as f32 + cell_w as f32 / 2.0 + offset.0,
            (row * cell_h) as f32 + cell_h as f32 / 2.0 + offset.1,
        );
        let (dx, dy) = (x - center.0, y - center.1);
        let half = code.size as f32 / 2.0;
        let u = (cos * dx + sin * dy) / pitch + half;
        let v = (-sin * dx + cos * dy) / pitch + half;
        let dark = u >= 0.0
            && v >= 0.0
            && (u as usize) < code.size
            && (v as usize) < code.size
            && code.pixels[v as usize * code.size + u as usize];
        if dark { DARK } else { LIGHT }
    };
    let mut pixels = vec![0_u8; width * height];
    // Rendering is the slow part of a synthetic camera, so it is spread over
    // every core, a band of rows each; a real camera costs the receiver none
    // of this.
    let threads = thread::available_parallelism().map_or(4, usize::from);
    let band = height.div_ceil(threads).max(1) * width;
    thread::scope(|scope| {
        for (index, chunk) in pixels.chunks_mut(band.max(1)).enumerate() {
            let first_row = index * band / width.max(1);
            let sample = &sample;
            scope.spawn(move || {
                for (offset, pixel) in chunk.iter_mut().enumerate() {
                    let (x, y) = ((offset % width) as f32, (first_row + offset / width) as f32);
                    // Four samples per pixel: module edges blur the way a
                    // lens blurs them.
                    let sum: f32 = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
                        .iter()
                        .map(|(ox, oy)| sample(x + ox, y + oy))
                        .sum();
                    *pixel = (sum / 4.0) as u8;
                }
            });
        }
    });
    LumaFrame {
        width,
        height,
        pixels,
    }
}

/// Stops the synthetic camera when dropped.
pub struct Guard(Arc<AtomicBool>, Option<thread::JoinHandle<()>>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
        if let Some(thread) = self.1.take() {
            let _ = thread.join();
        }
    }
}

/// Film `transfer` at `fps` camera frames per second, one sender frame per
/// camera frame, missing every `miss_every`-th one (0 for never) the way a
/// camera misses a flip. The picture drifts slowly, like a hand-held phone.
pub fn film(
    mut transfer: Transfer,
    framing: Framing,
    fps: f64,
    miss_every: u64,
    sink: FrameSink,
) -> (Guard, Negotiated) {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let codes = transfer.settings().grid_codes.max(1);
    let thread = thread::spawn(move || {
        let mut renderer = transfer.renderer();
        let period = Duration::from_secs_f64(1.0 / fps);
        let started = Instant::now();
        let mut index = 0_u64;
        while !stopped.load(Ordering::Acquire) {
            let frames: Vec<QrFrame> = (0..codes)
                .filter_map(|_| renderer.render(&transfer.next_payload()).ok())
                .collect();
            index += 1;
            if miss_every == 0 || !index.is_multiple_of(miss_every) {
                let t = started.elapsed().as_secs_f32();
                let drift = Framing {
                    offset: (
                        framing.offset.0 + 6.0 * (t * 0.7).sin(),
                        framing.offset.1 + 4.0 * (t * 0.9).cos(),
                    ),
                    degrees: framing.degrees + 0.4 * (t * 0.5).sin(),
                    ..framing
                };
                sink(Arc::new(camera_frame(&frames, drift)));
            }
            let due = started + period.mul_f64(index as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
        }
    });
    (
        Guard(stop, Some(thread)),
        Negotiated {
            name: "Synthetic camera".to_owned(),
            width: framing.width,
            height: framing.height,
            fps,
        },
    )
}
