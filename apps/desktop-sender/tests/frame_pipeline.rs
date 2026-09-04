//! The boundary between a transfer's payload bytes and the QR modules on
//! screen. Everything below this is `optical-core`'s, already pinned against
//! upstream; everything above it is pixels. This is the seam where a mistake
//! would produce a stream that looks perfect and decodes to nothing.

use desktop_sender::{
    qr::{Ecc, QUIET_ZONE_MODULES, QrFrame},
    transfer::{FRAME_BYTES_OPTIONS, StartError, TX_FPS_OPTIONS, Transfer, TransferSettings},
};
use optical_core::{
    MAX_FILE_BYTES,
    capacity::block_length,
    protocol::{FrameVerdict, HEADER_LEN, classify_frame, parse_frame, stream_identity},
};

fn settings() -> TransferSettings {
    TransferSettings::default()
}

#[test]
fn the_defaults_are_values_the_controls_actually_offer() {
    let defaults = settings();
    assert!(FRAME_BYTES_OPTIONS.contains(&defaults.frame_bytes));
    assert!(TX_FPS_OPTIONS.contains(&defaults.frames_per_second));
    assert_eq!(defaults.grid_codes, 1);
    assert_eq!(defaults.ecc, Ecc::Low);
    // Upstream pairs these two as its own fallback advice; inventing a third
    // number would make the sender recommend a setting nobody has measured.
    assert_eq!(defaults.frame_bytes, 1465);
    assert_eq!(defaults.frames_per_second, 24);
}

#[test]
fn successive_frames_share_one_stream_and_advance_by_one() {
    let mut transfer =
        Transfer::from_text("hello from desktop", settings(), 0x1234).expect("text packs");

    let first = transfer.next_payload();
    let second = transfer.next_payload();

    assert_eq!(classify_frame(&first), FrameVerdict::Valid);
    assert_eq!(classify_frame(&second), FrameVerdict::Valid);
    let first = parse_frame(&first).unwrap().unwrap();
    let second = parse_frame(&second).unwrap().unwrap();

    assert_eq!(first.header.seq, 0);
    assert_eq!(second.header.seq, 1);
    assert_eq!(
        stream_identity(first.header),
        stream_identity(second.header),
        "two frames of one transfer must be one stream to the receiver"
    );
    assert_eq!(first.header.session_id, 0x1234);
    assert_eq!(
        usize::from(first.header.block_len),
        block_length(settings().frame_bytes).unwrap()
    );
    assert_eq!(first.block.len(), usize::from(first.header.block_len));
}

#[test]
fn a_rendered_frame_is_a_square_matrix_inside_a_four_module_quiet_zone() {
    let mut transfer = Transfer::from_text("hello from desktop", settings(), 7).unwrap();
    let mut renderer = transfer.renderer();

    for _ in 0..3 {
        let payload = transfer.next_payload();
        let frame: QrFrame = renderer
            .render(&payload)
            .expect("a frame fits its QR version");
        assert_eq!(
            frame.size,
            frame.modules + 2 * QUIET_ZONE_MODULES,
            "the quiet zone is part of the raster, not the caller's problem"
        );
        assert_eq!(QUIET_ZONE_MODULES, 4);
        assert_eq!(frame.pixels.len(), frame.size * frame.size);
        // The quiet zone must be light on all four edges or a decoder will not
        // find the finder patterns.
        for index in 0..frame.size {
            assert!(!frame.pixels[index], "top edge is not quiet");
            assert!(
                !frame.pixels[(frame.size - 1) * frame.size + index],
                "bottom edge is not quiet"
            );
            assert!(!frame.pixels[index * frame.size], "left edge is not quiet");
            assert!(
                !frame.pixels[index * frame.size + frame.size - 1],
                "right edge is not quiet"
            );
        }
    }
}

#[test]
fn every_code_in_a_stream_has_identical_geometry() {
    // The receiver's grid tiling and the sender's own blitting both assume it,
    // and a version that drifts mid-stream produces codes of different sizes in
    // the same frame. Version locking is what prevents it.
    let mut transfer =
        Transfer::from_text(&"pinned geometry. ".repeat(400), settings(), 11).unwrap();
    let mut renderer = transfer.renderer();
    let first = renderer.render(&transfer.next_payload()).unwrap();
    for _ in 0..24 {
        let frame = renderer.render(&transfer.next_payload()).unwrap();
        assert_eq!(frame.modules, first.modules);
        assert_eq!(frame.version, first.version);
        assert_eq!(frame.size, first.size);
    }
}

#[test]
fn a_payload_too_dense_for_its_ecc_is_refused_before_a_stream_starts() {
    // 2953 bytes is exactly QR version 40 at ECC L. At ECC H it does not fit
    // any version, and the sender must say so rather than start a stream whose
    // first frame will fail to generate.
    let dense = TransferSettings {
        frame_bytes: 2953,
        ecc: Ecc::High,
        ..TransferSettings::default()
    };
    let error = Transfer::from_text("anything", dense, 1).expect_err("must not start");
    assert!(
        matches!(error, StartError::FrameTooDenseForEcc { .. }),
        "got {error:?}"
    );
    assert!(
        error.to_string().to_lowercase().contains("ecc"),
        "the message must point at the setting to change: {error}"
    );
}

#[test]
fn a_payload_beyond_the_block_ceiling_names_the_setting_that_fixes_it() {
    // `k` is a u16, so this is reachable well under the 64 MiB file limit —
    // and it is the failure a user cannot diagnose, because a stream that can
    // never complete looks exactly like a stream that is merely slow.
    let small_frames = TransferSettings {
        frame_bytes: 500,
        ..TransferSettings::default()
    };
    // A media type the packer declines to compress, so the container is the
    // size of the file. The check runs against the *container* — compression
    // changes the block count, and an earlier version of this test quietly
    // passed because 32 MiB of zeroes gzips to nothing.
    let payload = vec![0_u8; 32 * 1024 * 1024];
    let error = Transfer::from_file("big.jpg", "image/jpeg", &payload, small_frames, 1)
        .expect_err("32 MiB cannot be numbered in 478-byte blocks");
    let StartError::TooManyBlocks { suggestion, .. } = error else {
        panic!("got {error:?}");
    };
    let suggestion = suggestion.expect("an offered frame size does carry 40 MiB");
    assert!(
        FRAME_BYTES_OPTIONS.contains(&suggestion),
        "the suggestion must be a value the user can pick, got {suggestion}"
    );
    assert!(
        Transfer::from_file(
            "big.jpg",
            "image/jpeg",
            &payload,
            TransferSettings {
                frame_bytes: suggestion,
                ..TransferSettings::default()
            },
            1,
        )
        .is_ok(),
        "the suggested setting must actually resolve the error"
    );
}

#[test]
fn an_oversized_file_is_refused_before_anything_is_compressed() {
    let error = Transfer::from_file(
        "huge.bin",
        "application/octet-stream",
        &vec![0_u8; MAX_FILE_BYTES + 1],
        settings(),
        1,
    )
    .expect_err("over the limit");
    assert!(
        matches!(error, StartError::FileTooLarge { .. }),
        "{error:?}"
    );
}

#[test]
fn the_frame_size_leaves_exactly_the_header_behind() {
    for frame_bytes in FRAME_BYTES_OPTIONS {
        let mut transfer = Transfer::from_text(
            "a payload long enough to need more than one block. ",
            TransferSettings {
                frame_bytes,
                ..TransferSettings::default()
            },
            3,
        )
        .unwrap();
        let payload = transfer.next_payload();
        assert_eq!(
            payload.len(),
            frame_bytes,
            "a frame must be exactly the size the setting promised"
        );
        assert_eq!(
            payload.len() - HEADER_LEN,
            block_length(frame_bytes).unwrap()
        );
    }
}
