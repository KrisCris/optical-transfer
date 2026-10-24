//! The receive path closed in pixels: frames `optical-core` renders,
//! rasterised the way a camera sees a screen, decoded by zxing-cpp and fed to
//! the shared receiver until the file verifies.
//!
//! The protocol half of this loop is already covered without pixels in
//! `optical-wasm`'s sender-to-receiver test. What this adds is everything a
//! camera puts in between: fractional module pitch, a slight rotation, a grid
//! of codes in one frame, and the tracker and decode pool that have to find
//! them.

#![cfg(feature = "receive")]

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use desktop_sender::receive::{
    camera::LumaFrame,
    decoder::Decoder,
    session::Session,
    tracker::{Crop, Mode},
};
use optical_core::{
    qr::{Ecc, QrFrame},
    receiver::{ReceiverEvent, ReceiverState},
    transfer::{Transfer, TransferSettings},
};

const DARK: f32 = 40.0;
const LIGHT: f32 = 215.0;

/// Deterministic incompressible bytes, so the stream has real work in it.
fn noise(len: usize, mut seed: u64) -> Vec<u8> {
    (0..len)
        .map(|_| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as u8
        })
        .collect()
}

fn transfer(bytes: &[u8], codes: usize) -> Transfer {
    let settings = TransferSettings {
        frame_bytes: 2953,
        frames_per_second: 60,
        ecc: Ecc::Low,
        grid_codes: codes,
    };
    Transfer::from_file(
        "capture.bin",
        "application/octet-stream",
        bytes,
        settings,
        0x2b7d,
    )
    .unwrap()
}

/// Draw `codes` into a `width`×`height` luma frame, laid out `cols` wide,
/// each centred in its cell at `pitch` pixels per module and turned by
/// `degrees`. Every pixel averages four samples, so module edges blur the way
/// a lens blurs them.
fn camera_frame(
    codes: &[QrFrame],
    cols: usize,
    width: usize,
    height: usize,
    pitch: f32,
    degrees: f32,
) -> LumaFrame {
    let rows = codes.len().div_ceil(cols);
    let (cell_w, cell_h) = (width / cols, height / rows);
    let (sin, cos) = degrees.to_radians().sin_cos();
    let mut pixels = vec![LIGHT as u8; width * height];
    for (index, code) in codes.iter().enumerate() {
        let (left, top) = ((index % cols) * cell_w, (index / cols) * cell_h);
        let center = (
            left as f32 + cell_w as f32 / 2.0,
            top as f32 + cell_h as f32 / 2.0,
        );
        let half = code.size as f32 / 2.0;
        let module = |dx: f32, dy: f32| {
            let u = (cos * dx + sin * dy) / pitch + half;
            let v = (-sin * dx + cos * dy) / pitch + half;
            let dark = u >= 0.0
                && v >= 0.0
                && (u as usize) < code.size
                && (v as usize) < code.size
                && code.pixels[v as usize * code.size + u as usize];
            if dark { DARK } else { LIGHT }
        };
        for y in top..top + cell_h {
            for x in left..left + cell_w {
                let (dx, dy) = (x as f32 + 0.5 - center.0, y as f32 + 0.5 - center.1);
                let sum: f32 = [(-0.25, -0.25), (0.25, -0.25), (-0.25, 0.25), (0.25, 0.25)]
                    .iter()
                    .map(|(ox, oy)| module(dx + ox, dy + oy))
                    .sum();
                pixels[y * width + x] = (sum / 4.0) as u8;
            }
        }
    }
    LumaFrame {
        width,
        height,
        pixels,
    }
}

#[test]
fn rendered_frames_decode_through_zxing_into_a_verified_file() {
    let original = noise(48 * 1024, 7);
    let mut transfer = transfer(&original, 1);
    let mut renderer = transfer.renderer();
    let decoder = Decoder::new();
    let mut receiver = ReceiverState::new();

    for index in 0..2000 {
        let payload = transfer.next_payload();
        let code = renderer.render(&payload).unwrap();
        assert_eq!(code.version, 40, "2953 bytes at ECC L is a version 40 code");
        // A camera misses frames; the fountain must not care which.
        if index % 5 == 4 {
            continue;
        }
        let frame = camera_frame(&[code], 1, 640, 640, 3.1, 2.0);
        let whole = Crop {
            x: 0,
            y: 0,
            w: frame.width,
            h: frame.height,
        };
        let reply = decoder.decode(&frame.pixels, frame.width, frame.height, whole, true);
        assert_eq!(reply.symbols.len(), 1, "frame {index} did not decode");
        let symbol = &reply.symbols[0];
        assert_eq!(
            symbol.bytes, payload,
            "frame {index} decoded to other bytes"
        );
        assert!(
            symbol.bounds.w > 500.0 && symbol.bounds.w < 600.0,
            "the code's bounds are reported in frame pixels: {:?}",
            symbol.bounds
        );

        match receiver.add_frame(&symbol.bytes) {
            ReceiverEvent::Progress { .. } => {}
            ReceiverEvent::Complete {
                name,
                bytes,
                is_text,
                ..
            } => {
                assert_eq!(name, "capture.bin");
                assert!(!is_text);
                assert_eq!(bytes, original);
                return;
            }
            other => panic!("unexpected event at frame {index}: {other:?}"),
        }
    }
    panic!("the transfer never completed");
}

#[test]
fn a_crop_decodes_and_reports_bounds_in_frame_coordinates() {
    let mut transfer = transfer(&noise(8 * 1024, 3), 1);
    let payload = transfer.next_payload();
    let code = transfer.renderer().render(&payload).unwrap();
    let frame = camera_frame(&[code], 1, 1280, 960, 4.0, 1.0);
    let decoder = Decoder::new();
    let crop = Crop {
        x: 200,
        y: 60,
        w: 880,
        h: 840,
    };
    let reply = decoder.decode(&frame.pixels, frame.width, frame.height, crop, false);
    assert_eq!(reply.symbols.len(), 1);
    assert_eq!(reply.symbols[0].bytes, payload);
    let bounds = reply.symbols[0].bounds;
    // 177 modules at 4 px, centred in the frame.
    let center = (bounds.x + bounds.w / 2.0, bounds.y + bounds.h / 2.0);
    assert!(
        (center.0 - 640.0).abs() < 8.0 && (center.1 - 480.0).abs() < 8.0,
        "{bounds:?}"
    );
}

/// The whole session — tracker, pool, receiver, telemetry — on a 2×2 grid at
/// the web receiver's capture size, fed at camera pace.
#[test]
fn a_session_locks_onto_a_grid_and_receives_the_file() {
    let original = noise(160 * 1024, 11);
    let mut transfer = transfer(&original, 4);
    let mut renderer = transfer.renderer();
    let session = Session::start(4);
    let started = Instant::now();
    let mut most_locked = 0;

    loop {
        let codes: Vec<QrFrame> = (0..4)
            .map(|_| renderer.render(&transfer.next_payload()).unwrap())
            .collect();
        let frame = camera_frame(&codes, 2, 1280, 960, 2.55, 0.8);
        session.push(Arc::new(frame));
        thread::sleep(Duration::from_millis(16));

        let mut core = session.lock();
        if let Some(result) = &core.result {
            assert_eq!(result.name, "capture.bin");
            assert_eq!(result.bytes, original);
            let telemetry = core.telemetry.snapshot(session.now());
            assert!(telemetry.done);
            let totals = telemetry.totals;
            assert!(totals.full_scans >= 1);
            assert!(totals.crops > 0, "locked codes are read from crops");
            assert!(totals.symbols as usize >= core.stats.as_ref().unwrap().total_blocks);
            assert_eq!(most_locked, 4, "the tracker should lock all four codes");
            return;
        }
        assert!(core.failure.is_none(), "{:?}", core.failure);
        if core.tracker.mode() == Mode::Locked {
            most_locked = most_locked.max(core.tracker.decoded_count());
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "no file after 60 s: {:?}",
            core.stats
        );
    }
}
