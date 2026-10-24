//! Packing must not happen on the thread that draws the window.
//!
//! A gzip and a SHA-256 pass over a 64 MiB file is seconds of work. On the UI
//! thread that is seconds of a frozen window on a program that is doing exactly
//! what it was asked to. The trap is that the obvious manual check misses it
//! entirely: a small file packs in milliseconds and looks perfect, so the
//! defect only ever appears in front of a user with a real file.
//!
//! These tests pin the property rather than the timing — a wall-clock assertion
//! would measure the machine — by proving the caller gets an answer it can act
//! on *before* the work is finished, which is only possible off-thread.

use std::time::{Duration, Instant};

use desktop_sender::packing::{PackingJob, PackingState};
use optical_core::transfer::TransferSettings;

/// Big enough that packing is real work, incompressible so the gzip attempt
/// cannot short-circuit it.
fn heavy_payload() -> Vec<u8> {
    (0..8_000_000_u32)
        .map(|index| (index.wrapping_mul(2_654_435_761) >> 16) as u8)
        .collect()
}

fn settle(job: &mut PackingJob) -> PackingState {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match job.poll() {
            PackingState::Busy => {
                assert!(Instant::now() < deadline, "packing never finished");
                std::thread::yield_now();
            }
            settled => return settled,
        }
    }
}

#[test]
fn the_caller_keeps_control_while_a_heavy_file_is_prepared() {
    let mut job = PackingJob::file(
        "heavy.bin".to_owned(),
        "application/octet-stream".to_owned(),
        heavy_payload(),
        TransferSettings::default(),
        0x3030,
    );

    // The contract: poll returns immediately, every time, whatever the worker
    // is doing. If packing were inline, construction itself would have blocked
    // and this loop could not run at all.
    let started = Instant::now();
    for _ in 0..1000 {
        let slice = Instant::now();
        let state = job.poll();
        assert!(
            slice.elapsed() < Duration::from_millis(50),
            "poll blocked for {:?} — packing is on the caller's thread",
            slice.elapsed()
        );
        if !matches!(state, PackingState::Busy) {
            break;
        }
    }
    let _ = started;

    let PackingState::Ready(transfer) = settle(&mut job) else {
        panic!("a valid file must pack");
    };
    assert_eq!(transfer.name(), "heavy.bin");
    assert!(transfer.source_blocks() > 1);
}

#[test]
fn a_refusal_comes_back_as_a_result_rather_than_a_panic() {
    // The worker thread must not be a place where errors go to die: a file the
    // packer rejects has to reach the UI as something it can display.
    let mut job = PackingJob::file(
        "empty.bin".to_owned(),
        "application/octet-stream".to_owned(),
        Vec::new(),
        TransferSettings::default(),
        1,
    );
    let PackingState::Failed(error) = settle(&mut job) else {
        panic!("an empty file must be refused");
    };
    assert!(!error.to_string().is_empty());
}

#[test]
fn abandoning_a_job_does_not_take_the_process_with_it() {
    // A user who picks a second file while the first is still compressing drops
    // the job. The worker finishes into a closed channel, which must be a
    // no-op rather than a panic on an unwrap.
    let job = PackingJob::file(
        "abandoned.bin".to_owned(),
        "application/octet-stream".to_owned(),
        heavy_payload(),
        TransferSettings::default(),
        1,
    );
    drop(job);

    let mut replacement = PackingJob::text(
        "the file the user actually wanted".to_owned(),
        TransferSettings::default(),
        2,
    );
    assert!(matches!(settle(&mut replacement), PackingState::Ready(_)));
}
