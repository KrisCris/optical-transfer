//! The loop closed in software: this sender's frames into that receiver.
//!
//! Both halves are already tested against upstream separately, which is the
//! part that proves compatibility. This is the part that proves they were
//! wired together — a sender that packs correctly and a receiver that decodes
//! correctly can still fail to talk if the sender's header, block length or
//! sequence cadence disagrees with what the receiver expects, and nothing in
//! either crate's own tests would notice.
//!
//! What is deliberately *not* here is pixels. Everything from
//! `QrFrame::pixels` to a camera is Task 7's, and pretending otherwise with a
//! synthetic rasterise-then-reparse would test nothing but this file.

use desktop_sender::{
    qr::Ecc,
    transfer::{FRAME_BYTES_OPTIONS, Transfer, TransferSettings},
};
use optical_wasm::{ReceiverEvent, ReceiverState};

/// Drive a transfer into a receiver, dropping frames as a camera would.
///
/// Returns the completion event, or panics with what actually happened.
fn run(mut transfer: Transfer, drop_every: u32, limit: u32) -> ReceiverEvent {
    let mut receiver = ReceiverState::new();
    for index in 0..limit {
        let payload = transfer.next_payload();
        if drop_every != 0 && index % drop_every == drop_every - 1 {
            continue;
        }
        match receiver.add_frame(&payload) {
            ReceiverEvent::Progress { .. } => {}
            event @ ReceiverEvent::Complete { .. } => return event,
            other => panic!("unexpected event at frame {index}: {other:?}"),
        }
    }
    panic!("the transfer never completed within {limit} frames");
}

#[test]
fn a_text_snippet_survives_the_whole_pipeline() {
    let text = "Optical transfer — 光学传输 ✅\n".repeat(300);
    let transfer = Transfer::from_text(&text, TransferSettings::default(), 0x51c3).unwrap();

    let ReceiverEvent::Complete {
        name,
        media_type,
        bytes,
        is_text,
    } = run(transfer, 6, 20_000)
    else {
        unreachable!("run only returns Complete")
    };
    assert_eq!(name, "snippet.txt");
    assert_eq!(media_type, "application/vnd.decimen.snippet");
    assert!(is_text);
    assert_eq!(String::from_utf8(bytes).unwrap(), text);
}

#[test]
fn a_binary_file_survives_the_whole_pipeline() {
    // Incompressible, so the container is the file and the block count is real.
    let payload: Vec<u8> = (0..300_000_u32)
        .map(|index| (index.wrapping_mul(2_654_435_761) >> 16) as u8)
        .collect();
    let transfer = Transfer::from_file(
        "réponse finale.bin",
        "application/octet-stream",
        &payload,
        TransferSettings::default(),
        0x0bad,
    )
    .unwrap();

    let ReceiverEvent::Complete {
        name,
        media_type,
        bytes,
        is_text,
    } = run(transfer, 6, 40_000)
    else {
        unreachable!("run only returns Complete")
    };
    assert_eq!(
        name, "réponse finale.bin",
        "a Unicode filename must survive"
    );
    assert_eq!(media_type, "application/octet-stream");
    assert!(!is_text);
    assert_eq!(bytes, payload);
}

#[test]
fn every_offered_setting_produces_a_stream_the_receiver_can_finish() {
    // A setting the sender offers but the receiver cannot complete would be a
    // control that silently does not work.
    let text = "settings sweep. ".repeat(500);
    for frame_bytes in FRAME_BYTES_OPTIONS {
        for ecc in Ecc::ALL {
            let settings = TransferSettings {
                frame_bytes,
                ecc,
                ..TransferSettings::default()
            };
            let transfer = match Transfer::from_text(&text, settings, 0x2222) {
                Ok(transfer) => transfer,
                // Refused up front with a message is the correct answer for a
                // density the ECC cannot carry; silence would not be.
                Err(error) => {
                    assert!(
                        !error.to_string().is_empty(),
                        "a refusal must say something"
                    );
                    continue;
                }
            };
            let ReceiverEvent::Complete { bytes, .. } = run(transfer, 5, 60_000) else {
                unreachable!("run only returns Complete")
            };
            assert_eq!(
                String::from_utf8(bytes).unwrap(),
                text,
                "{frame_bytes} bytes at ECC {} lost data",
                ecc.label()
            );
        }
    }
}

#[test]
fn a_receiver_joining_mid_carousel_still_completes() {
    // There is no handshake and none is needed: the carousel repeats forever,
    // so a phone pointed at a screen that has been streaming for a while has to
    // work exactly as well as one that caught the first frame.
    let text = "joined late. ".repeat(400);
    let mut transfer = Transfer::from_text(&text, TransferSettings::default(), 0x7777).unwrap();
    // Burn most of a cycle before the receiver looks at the screen.
    for _ in 0..(transfer.cycle_frames() * 3 / 2) {
        let _ = transfer.next_payload();
    }

    let ReceiverEvent::Complete { bytes, .. } = run(transfer, 6, 20_000) else {
        unreachable!("run only returns Complete")
    };
    assert_eq!(String::from_utf8(bytes).unwrap(), text);
}
