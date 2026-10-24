//! The stage's timing and geometry, without a window.

use std::time::{Duration, Instant};

use desktop_sender::stream::{FlipClock, FrameSource, best_grid};
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

#[test]
fn codes_times_fps_is_not_capped_by_the_refresh_rate() {
    let start = Instant::now();
    let mut clock = FlipClock::new(60, 4, start);
    let flips = run(&mut clock, start, 60.0, 2.0);
    // 240 codes a second on a 60 Hz display; one flip per vsync would be 120.
    assert!((470..=484).contains(&flips.len()), "{} flips", flips.len());
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

    let mut clock = FlipClock::new(120, 1, start);
    let flips = run(&mut clock, start, 144.0, 2.0);
    assert!(
        (236..=242).contains(&flips.len()),
        "a 144 Hz display reaches 120: {} flips",
        flips.len()
    );
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
    let source = FrameSource::spawn(transfer);
    assert_eq!(source.info.session_id, 7);
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
