//! The boundary between a transfer's payload bytes and the QR modules on
//! screen. Everything below this is `optical-core`'s, already pinned against
//! upstream; everything above it is pixels. This is the seam where a mistake
//! would produce a stream that looks perfect and decodes to nothing.

use optical_core::{
    MAX_FILE_BYTES, V3_MAX_FILE_BYTES,
    capacity::block_length,
    protocol::{
        FrameVerdict, HEADER_LEN, V4_HEADER_LEN, Wire, classify_frame, parse_frame, parse_frame_v4,
        stream_identity, stream_identity_v4,
    },
    qr::{Ecc, QUIET_ZONE_MODULES, QrFrame, compose},
    transfer::{FRAME_BYTES_OPTIONS, StartError, TX_FPS_OPTIONS, Transfer, TransferSettings},
};

fn settings() -> TransferSettings {
    TransferSettings::default()
}

fn v3() -> TransferSettings {
    TransferSettings {
        wire: Wire::V3,
        ..TransferSettings::default()
    }
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
    // This build's own format unless someone asks for upstream's.
    assert_eq!(defaults.wire, Wire::V4);
    assert!(!defaults.color);
}

#[test]
fn successive_v4_frames_share_one_stream_and_advance_by_one() {
    let mut transfer =
        Transfer::from_text("hello from desktop", settings(), 0x1234).expect("text packs");
    let first = transfer.next_payload();
    let second = transfer.next_payload();
    assert_eq!(classify_frame(&first), FrameVerdict::Valid(Wire::V4));
    let first = parse_frame_v4(&first).unwrap();
    let second = parse_frame_v4(&second).unwrap();

    // One source symbol: frame 0 is it, frame 1 the first repair symbol.
    assert_eq!((first.header.block, first.header.symbol), (0, 0));
    assert_eq!((second.header.block, second.header.symbol), (0, 1));
    assert_eq!(
        stream_identity_v4(first.header, first.symbol.len()),
        stream_identity_v4(second.header, second.symbol.len()),
    );
    assert_eq!(transfer.stream_id(), first.header.checksum);
    assert_eq!(
        first.symbol.len(),
        first.header.total_len as usize,
        "a payload that fits one symbol is sent at its own size, not padded"
    );
    assert_eq!(
        transfer.cycle_frames(),
        None,
        "v4 has no carousel to repeat"
    );
}

#[test]
fn the_same_file_at_the_same_settings_is_the_same_v4_stream() {
    // No session id on v4: a sender restarted on the same file continues the
    // receiver's transfer instead of starting a new one. Only the content and
    // the symbol size name the stream.
    let mut first = Transfer::from_text("restart me", settings(), 1).unwrap();
    let mut again = Transfer::from_text("restart me", settings(), 2).unwrap();
    assert_eq!(first.next_payload(), again.next_payload());
    let other = Transfer::from_text("restart you", settings(), 1).unwrap();
    assert_ne!(first.stream_id(), other.stream_id());
}

#[test]
fn successive_v3_frames_share_one_stream_and_advance_by_one() {
    let mut transfer = Transfer::from_text("hello from desktop", v3(), 0x1234).expect("text packs");

    let first = transfer.next_payload();
    let second = transfer.next_payload();

    assert_eq!(classify_frame(&first), FrameVerdict::Valid(Wire::V3));
    assert_eq!(classify_frame(&second), FrameVerdict::Valid(Wire::V3));
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
    assert_eq!(first.header.source_blocks, 1);
    assert_eq!(
        usize::from(first.header.block_len),
        first.header.total_len as usize,
        "a payload that fits one block is sent at its own size, not padded"
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
        ..v3()
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
                ..v3()
            },
            1,
        )
        .is_ok(),
        "the suggested setting must actually resolve the error"
    );
    // Wire v4 numbers symbols in 24 bits per block: the same file streams at
    // the smallest frame size.
    assert!(
        Transfer::from_file(
            "big.jpg",
            "image/jpeg",
            &payload,
            TransferSettings {
                frame_bytes: 500,
                ..settings()
            },
            1
        )
        .is_ok()
    );
}

#[test]
fn an_oversized_file_is_refused_before_anything_is_compressed() {
    for (settings, limit) in [(settings(), MAX_FILE_BYTES), (v3(), V3_MAX_FILE_BYTES)] {
        let error = Transfer::from_file(
            "huge.bin",
            "application/octet-stream",
            &vec![0_u8; limit + 1],
            settings,
            1,
        )
        .expect_err("over the limit");
        assert_eq!(
            error,
            StartError::FileTooLarge {
                bytes: limit + 1,
                limit
            }
        );
    }
    const { assert!(MAX_FILE_BYTES > V3_MAX_FILE_BYTES, "v4 raised the limit") };
}

#[test]
fn a_file_at_v3s_limit_that_packs_past_it_is_refused_by_name() {
    // Video is stored, not compressed, so the container is the file plus its
    // header, name and media type: over upstream's 64 MiB, which limits the
    // container, though the file is not.
    let error = Transfer::from_file(
        "full.mp4",
        "video/mp4",
        &vec![0_u8; V3_MAX_FILE_BYTES],
        v3(),
        1,
    )
    .expect_err("a container over v3's limit");
    let StartError::ContainerTooLarge { bytes, limit } = error else {
        panic!("got {error:?}");
    };
    assert_eq!(limit, V3_MAX_FILE_BYTES);
    assert!(bytes > limit && bytes < limit + 1024, "{bytes}");
    assert!(error.to_string().contains("wire v4"), "{error}");
}

#[test]
fn a_color_code_is_three_frames_of_one_geometry() {
    let color = TransferSettings {
        color: true,
        ..settings()
    };
    assert_eq!(color.layers(), 3);
    assert_eq!(color.wire_rate(), 3.0 * settings().wire_rate());
    let mut transfer = Transfer::from_text(&"three channels. ".repeat(300), color, 9).unwrap();
    let mut renderer = transfer.renderer();
    let layers: Vec<QrFrame> = transfer
        .next_layers()
        .iter()
        .map(|payload| renderer.render(payload).unwrap())
        .collect();
    assert_eq!(layers.len(), 3);
    let modules = compose(&layers);
    assert_eq!(modules.len(), layers[0].pixels.len());
    for (index, module) in modules.iter().enumerate() {
        for (channel, layer) in layers.iter().enumerate() {
            assert_eq!(module >> channel & 1 == 1, layer.pixels[index]);
        }
    }
    // Function patterns coincide, so the top-left finder's corner is black in
    // every channel and the quiet zone white.
    let corner = QUIET_ZONE_MODULES * layers[0].size + QUIET_ZONE_MODULES;
    assert_eq!(modules[corner], 0b111);
    assert_eq!(modules[0], 0);
    // Black and white is the one-layer case of the same encoding.
    assert!(
        compose(&layers[..1])
            .iter()
            .all(|module| *module == 0 || *module == 0b111)
    );
}

#[test]
fn the_frame_size_is_a_ceiling_and_blocks_are_balanced_under_it() {
    // Incompressible, so the container is about as long as the text.
    let mut state = 0x9e37_79b9_u32;
    let text: String = (0..9000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            char::from(b'!' + (state % 90) as u8)
        })
        .collect();
    for frame_bytes in FRAME_BYTES_OPTIONS {
        let settings = TransferSettings {
            frame_bytes,
            ..v3()
        };
        let mut transfer = Transfer::from_text(&text, settings, 3).unwrap();
        let payload = transfer.next_payload();
        let frame = parse_frame(&payload).unwrap().unwrap();
        let total = frame.header.total_len as usize;
        let ceiling = block_length(frame_bytes, Wire::V3).unwrap();
        let blocks = total.div_ceil(ceiling);

        assert!(
            payload.len() <= frame_bytes,
            "a frame may never outgrow its setting"
        );
        assert_eq!(
            payload.len() - HEADER_LEN,
            usize::from(frame.header.block_len)
        );
        assert_eq!(
            usize::from(frame.header.source_blocks),
            blocks,
            "shrinking blocks must not add any"
        );
        assert_eq!(usize::from(frame.header.block_len), total.div_ceil(blocks));

        // The same rule on v4, where the frame length is the symbol size.
        let mut transfer = Transfer::from_text(
            &text,
            TransferSettings {
                frame_bytes,
                ..TransferSettings::default()
            },
            3,
        )
        .unwrap();
        let payload = transfer.next_payload();
        let frame = parse_frame_v4(&payload).unwrap();
        let total = frame.header.total_len as usize;
        let symbols = total.div_ceil(block_length(frame_bytes, Wire::V4).unwrap());
        assert!(payload.len() <= frame_bytes);
        assert_eq!(payload.len() - V4_HEADER_LEN, total.div_ceil(symbols));
        assert_eq!(transfer.source_blocks() as usize, symbols);
    }
}
