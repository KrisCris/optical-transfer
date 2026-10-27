//! The stage's timing and geometry, without a window.

use std::time::{Duration, Instant};

use desktop_sender::stream::{FlipClock, FrameSource, best_grid, hold_frames};
use eframe::egui::Color32;
use optical_core::transfer::{Transfer, TransferSettings};

/// Tick `clock` at `hz` for `seconds`, with an endless supply of frames.
fn run(clock: &mut FlipClock, start: Instant, hz: f64, seconds: f64) -> Vec<(Instant, usize)> {
    let mut flips = Vec::new();
    let frames = (hz * seconds) as u32;
    for index in 0..frames {
        let now = start + Duration::from_secs_f64(f64::from(index) / hz);
        clock.tick(now, || Some(()), |cell, ()| flips.push((now, cell)));
    }
    flips
}

/// For every cell, how many `hz` refreshes each of its codes stayed up,
/// leaving out flips in the first 200 ms while the refresh is still measured.
fn holds(flips: &[(Instant, usize)], start: Instant, hz: f64) -> Vec<u32> {
    let mut last = std::collections::HashMap::new();
    let mut frames = Vec::new();
    for (at, cell) in flips {
        if let Some(previous) = last.insert(*cell, *at)
            && at.duration_since(start) >= Duration::from_millis(200)
        {
            frames.push((at.duration_since(previous).as_secs_f64() * hz).round() as u32);
        }
    }
    frames
}

#[test]
fn codes_times_fps_is_not_capped_by_the_refresh_rate() {
    let start = Instant::now();
    let mut clock = FlipClock::new(60, 4, start);
    let flips = run(&mut clock, start, 60.0, 2.0);
    // 240 codes a second on a 60 Hz display; one flip per vsync would be 120.
    // The first frame flips three cells, every later one all four.
    assert!((478..=480).contains(&flips.len()), "{} flips", flips.len());
    let cells: Vec<_> = flips.iter().take(8).map(|(_, cell)| *cell).collect();
    assert_eq!(cells, [0, 1, 2, 3, 0, 1, 2, 3], "cells flip in turn");
    assert!((55.0..65.0).contains(&clock.refresh_hz().unwrap()));
}

#[test]
fn a_rate_above_the_refresh_rate_runs_at_the_refresh_rate() {
    let start = Instant::now();
    let mut clock = FlipClock::new(120, 1, start);
    let flips = run(&mut clock, start, 60.0, 2.0);
    // A second flip inside one vsync would paint over a code nobody saw.
    let mut per_frame = std::collections::HashMap::new();
    for (at, _) in &flips {
        *per_frame.entry(*at).or_insert(0) += 1;
    }
    assert!(
        per_frame.values().all(|count| *count == 1),
        "a cell flipped twice in one frame"
    );
    assert!((118..=121).contains(&flips.len()), "{} flips", flips.len());

    // One 144 Hz refresh is short of 1/120 s, so each code stays up two: 72 a
    // second, evenly, rather than 120 alternating one refresh and two.
    let mut clock = FlipClock::new(120, 1, start);
    let flips = run(&mut clock, start, 144.0, 2.0);
    assert!(
        (143..=145).contains(&flips.len()),
        "a 144 Hz display holds 120 fps codes two refreshes: {} flips",
        flips.len()
    );
    assert!(
        holds(&flips, start, 144.0)
            .iter()
            .all(|frames| *frames == 2)
    );
}

#[test]
fn a_code_stays_up_the_fewest_whole_refreshes_not_much_shorter_than_its_interval() {
    let frames = |fps, hz: f64| hold_frames(fps, Duration::from_secs_f64(1.0 / hz));
    assert_eq!(frames(36, 144.0), 4, "28 ms");
    assert_eq!(frames(36, 60.0), 2, "33 ms: 30 a second");
    assert_eq!(frames(36, 30.0), 1, "a device drawing 30 frames a second");
    assert_eq!(frames(36, 120.0), 4);
    assert_eq!(frames(36, 165.0), 5);
    assert_eq!(frames(36, 75.0), 2);
    assert_eq!(frames(60, 144.0), 3, "never three and two in turn");
    assert_eq!(frames(60, 60.0), 1);
    assert_eq!(frames(24, 60.0), 3);
    assert_eq!(frames(30, 144.0), 5);
    assert_eq!(frames(72, 144.0), 2);
    assert_eq!(frames(120, 60.0), 1, "never less than one refresh");
}

#[test]
fn at_144_hz_every_36_fps_code_stays_up_four_refreshes_and_60_fps_three() {
    for (fps, expected, per_second) in [(36, 4, 36), (60, 3, 48)] {
        let start = Instant::now();
        let mut clock = FlipClock::new(fps, 1, start);
        let flips = run(&mut clock, start, 144.0, 2.0);
        let up = holds(&flips, start, 144.0);
        assert!(
            !up.is_empty() && up.iter().all(|frames| *frames == expected),
            "{fps} fps: {up:?}"
        );
        assert!(
            flips.len().abs_diff(2 * per_second) <= 2,
            "{fps} fps: {} in 2 s",
            flips.len()
        );
    }
}

#[test]
fn thirty_six_fps_runs_at_30_on_a_60_hz_display_and_on_one_drawing_30_frames() {
    for (hz, expected) in [(60.0, 2), (30.0, 1)] {
        let start = Instant::now();
        let mut clock = FlipClock::new(36, 1, start);
        let flips = run(&mut clock, start, hz, 2.0);
        assert!(
            holds(&flips, start, hz)
                .iter()
                .all(|frames| *frames == expected),
            "{hz} Hz"
        );
        assert!(
            flips.len().abs_diff(60) <= 2,
            "{hz} Hz: {} in 2 s",
            flips.len()
        );
    }
}

#[test]
fn four_codes_at_144_hz_flip_one_a_refresh_each_up_four() {
    let start = Instant::now();
    let mut clock = FlipClock::new(36, 4, start);
    let flips = run(&mut clock, start, 144.0, 2.0);
    let mut per_frame = std::collections::HashMap::new();
    for (at, _) in &flips {
        if at.duration_since(start) >= Duration::from_millis(200) {
            *per_frame.entry(*at).or_insert(0) += 1;
        }
    }
    assert!(
        per_frame.values().all(|count| *count == 1),
        "two cells flipped together"
    );
    assert!(
        holds(&flips, start, 144.0)
            .iter()
            .all(|frames| *frames == 4)
    );
    assert!(flips.len().abs_diff(288) <= 4, "{} flips", flips.len());
}

#[test]
fn a_frame_the_window_skips_lengthens_a_code_and_never_shortens_the_next() {
    let start = Instant::now();
    let mut clock = FlipClock::new(36, 1, start);
    let mut flips = Vec::new();
    // Every seventh display frame never reaches the clock.
    for index in (0..288_u32).filter(|index| index % 7 != 6) {
        let now = start + Duration::from_secs_f64(f64::from(index) / 144.0);
        clock.tick(now, || Some(()), |cell, ()| flips.push((now, cell)));
    }
    let up = holds(&flips, start, 144.0);
    assert!(up.iter().all(|frames| *frames >= 4), "{up:?}");
}

#[test]
fn cells_flip_staggered_when_the_display_is_fast_enough() {
    let start = Instant::now();
    let mut clock = FlipClock::new(30, 2, start);
    let flips = run(&mut clock, start, 120.0, 1.0);
    let mut per_frame = std::collections::HashMap::new();
    for (at, _) in &flips {
        *per_frame.entry(*at).or_insert(0) += 1;
    }
    assert!(
        per_frame.values().all(|count| *count == 1),
        "two cells flipped together"
    );
    assert!((58..=62).contains(&flips.len()), "{} flips", flips.len());
}

#[test]
fn a_stall_resumes_at_rate_instead_of_flushing_a_backlog() {
    let start = Instant::now();
    let mut clock = FlipClock::new(30, 1, start);
    run(&mut clock, start, 60.0, 1.0);
    let before = clock.shown;
    let mut flipped = 0;
    clock.tick(
        start + Duration::from_secs(4),
        || Some(()),
        |_, ()| flipped += 1,
    );
    assert_eq!(flipped, 1, "one frame after the stall, not ninety");
    assert_eq!(clock.shown, before + 1);
    assert!(clock.worst_stall >= Duration::from_secs(2));
}

#[test]
fn an_empty_queue_is_counted_and_retried_rather_than_skipped() {
    let start = Instant::now();
    let mut clock = FlipClock::new(60, 1, start);
    clock.tick(start, || None::<()>, |_, ()| unreachable!());
    assert_eq!(clock.starved, 1);
    assert_eq!(clock.shown, 0);
    let mut placed = 0;
    clock.tick(
        start + Duration::from_millis(20),
        || Some(()),
        |_, ()| placed += 1,
    );
    assert_eq!(placed, 1);
}

#[test]
fn reconfiguring_keeps_the_stream_position() {
    let start = Instant::now();
    let mut clock = FlipClock::new(30, 1, start);
    run(&mut clock, start, 60.0, 1.0);
    let shown = clock.shown;
    clock.configure(60, 4);
    assert_eq!(clock.shown, shown);
    assert_eq!(clock.codes(), 4);
}

#[test]
fn the_grid_follows_the_window() {
    assert_eq!(best_grid(2, 185, 1600.0, 900.0), (2, 1));
    assert_eq!(best_grid(2, 185, 700.0, 1200.0), (1, 2));
    assert_eq!(best_grid(4, 185, 1000.0, 1000.0), (2, 2));
    assert_eq!(best_grid(6, 185, 1920.0, 1080.0), (3, 2));
    assert_eq!(best_grid(9, 185, 1000.0, 1000.0), (3, 3));
    assert_eq!(best_grid(1, 185, 10.0, 10.0), (1, 1));
}

#[test]
fn the_worker_streams_real_frames_in_order_and_stops_when_dropped() {
    let transfer =
        Transfer::from_text(&"worker ".repeat(2000), TransferSettings::default(), 7).unwrap();
    let stream_id = transfer.stream_id();
    let source = FrameSource::spawn(transfer);
    assert_eq!(source.info.stream_id, stream_id);
    assert_eq!(source.info.cycle_frames, None, "v4 has no carousel");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut frames = Vec::new();
    while frames.len() < 5 {
        assert!(Instant::now() < deadline, "the worker produced nothing");
        match source.try_next() {
            Some(frame) => frames.push(frame.expect("frames render")),
            None => std::thread::yield_now(),
        }
    }
    let size = frames[0].size;
    assert!(
        frames.iter().all(|frame| frame.size == size),
        "one version for the whole stream"
    );
    assert_eq!(frames[0].image.size, [size, size]);
    // Dropping the source closes the channel; the worker's next send fails
    // and it exits instead of encoding forever into nothing.
    drop(source);
}

#[test]
fn a_colour_stream_paints_three_frames_into_each_code() {
    let settings = TransferSettings {
        color: true,
        ..TransferSettings::default()
    };
    let transfer = Transfer::from_text(&"colour ".repeat(3000), settings, 7).unwrap();
    let source = FrameSource::spawn(transfer);
    let deadline = Instant::now() + Duration::from_secs(10);
    let frame = loop {
        assert!(Instant::now() < deadline, "the worker produced nothing");
        if let Some(frame) = source.try_next() {
            break frame.expect("frames render");
        }
        std::thread::yield_now();
    };
    let pixels = &frame.image.pixels;
    // Finders are dark in every layer, so black; data modules take every
    // mixture of the three channels.
    assert_eq!(
        pixels.iter().find(|pixel| **pixel != Color32::WHITE),
        Some(&Color32::BLACK)
    );
    let mixtures: std::collections::HashSet<_> = pixels.iter().copied().collect();
    assert_eq!(mixtures.len(), 8, "all eight channel mixtures appear");
}
