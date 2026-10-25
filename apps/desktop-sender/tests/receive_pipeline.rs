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
    decoder::Decoder,
    session::Session,
    synthetic::{Framing, camera_frame, colour_frame},
    tracker::{Crop, Mode},
};
use optical_core::{
    qr::{Ecc, QrFrame},
    receiver::{ReceiverEvent, ReceiverState},
    transfer::{Transfer, TransferSettings},
};

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
        ..TransferSettings::default()
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

fn framing(columns: usize, width: usize, height: usize, pitch: f32, degrees: f32) -> Framing {
    Framing {
        width,
        height,
        columns,
        pitch,
        degrees,
        offset: (0.0, 0.0),
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
        let frame = camera_frame(&[code], framing(1, 640, 640, 3.1, 2.0));
        let whole = Crop {
            x: 0,
            y: 0,
            w: frame.width,
            h: frame.height,
        };
        let reply = decoder.decode(&frame, whole, true);
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
    let frame = camera_frame(&[code], framing(1, 1280, 960, 4.0, 1.0));
    let decoder = Decoder::new();
    let crop = Crop {
        x: 200,
        y: 60,
        w: 880,
        h: 840,
    };
    let reply = decoder.decode(&frame, crop, false);
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
        let frame = camera_frame(&codes, framing(2, 1280, 960, 2.55, 0.8));
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

/// A colour code through a camera that mixes its channels and keeps colour
/// at half resolution: found by a full scan, then read from crops, one
/// symbol per layer either way.
#[test]
fn a_colour_code_is_unmixed_into_its_three_layers() {
    let mut transfer = transfer(&noise(64 * 1024, 5), 1);
    let mut renderer = transfer.renderer();
    let payloads: Vec<Vec<u8>> = (0..3).map(|_| transfer.next_payload()).collect();
    let layers: Vec<QrFrame> = payloads
        .iter()
        .map(|payload| renderer.render(payload).unwrap())
        .collect();
    // 185 modules at 4.6 px fill most of a 1280×960 picture's height.
    let frame = colour_frame(&layers, framing(1, 1280, 960, 4.6, 1.5), 0.2);
    let decoder = Decoder::new();
    let whole = Crop {
        x: 0,
        y: 0,
        w: frame.width,
        h: frame.height,
    };
    let read = |reply: desktop_sender::receive::decoder::Reply| {
        let mut bytes: Vec<Vec<u8>> = reply
            .symbols
            .iter()
            .inspect(|symbol| assert!(symbol.colour))
            .map(|symbol| symbol.bytes.clone())
            .collect();
        bytes.sort();
        let mut expected = payloads.clone();
        expected.sort();
        assert_eq!(bytes, expected, "every layer, each once");
    };
    let scan = decoder.decode(&frame, whole, true);
    assert!(scan.sightings.is_empty(), "the sighting became a decode");
    read(scan);
    let crop = Crop {
        x: 160,
        y: 0,
        w: 960,
        h: 960,
    };
    read(decoder.decode(&frame, crop, false));

    // Without chroma the most a receiver gets is the layer luma favours:
    // green, which Rec. 709 luma is seven tenths of.
    let grey = desktop_sender::receive::camera::LumaFrame {
        chroma: Vec::new(),
        ..frame
    };
    let scan = decoder.decode(&grey, whole, true);
    assert!(
        scan.symbols.len() + scan.sightings.len() >= 1,
        "the code is seen"
    );
    assert!(
        scan.symbols
            .iter()
            .all(|symbol| !symbol.colour && symbol.bytes == payloads[1]),
        "at most the green layer, read as an ordinary code"
    );
}
