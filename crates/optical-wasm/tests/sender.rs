use optical_wasm::{SendOptions, SenderState};

/// The browser's controls: bytes per code, frame rate and ECC, with v4 in
/// black and white unless a test says otherwise.
fn options(frame_bytes: usize, fps: u32, ecc: &str) -> SendOptions<'_> {
    SendOptions {
        frame_bytes,
        fps,
        ecc,
        session_id: 1,
        wire: 4,
        color: false,
    }
}

#[test]
fn a_file_prepares_an_endless_stream_of_constant_qr_geometry() {
    let mut sender = SenderState::new();
    sender
        .add_file(
            "hello.txt",
            "text/plain",
            b"hello from the browser".to_vec(),
        )
        .unwrap();
    let info = sender.prepare_files(options(1465, 24, "L")).unwrap();
    assert_eq!(info.name, "hello.txt");
    assert_eq!(info.original_bytes, 22);
    assert_eq!(info.wire, 4);
    assert_eq!(info.layers, 1);
    assert_eq!(info.cycle_frames, None, "v4 never repeats a frame");
    assert!(info.qr_size > 8);
    for _ in 0..5 {
        let frame = sender.next_frame().unwrap();
        assert_eq!(frame.size, info.qr_size);
        assert_eq!(frame.version, info.qr_version);
        assert!(frame.pixels.iter().any(|pixel| *pixel));
    }
    assert!(sender.add_file("late.txt", "text/plain", vec![1]).is_err());
}

#[test]
fn multiple_files_are_prepared_as_one_zip_transfer() {
    let mut sender = SenderState::new();
    sender
        .add_file("one.jpg", "image/jpeg", vec![1, 2, 3])
        .unwrap();
    sender
        .add_file("two.txt", "text/plain", vec![4, 5])
        .unwrap();
    let info = sender.prepare_files(options(500, 15, "L")).unwrap();
    assert_eq!(info.name, "transfer.zip");
    assert!(info.original_bytes > 5);
    assert_eq!(info.file_count, 2);
    assert!(sender.next_frame().is_ok());
}

#[test]
fn text_and_invalid_settings_are_handled_before_streaming() {
    let mut sender = SenderState::new();
    assert!(sender.next_frame().is_err());
    assert!(sender.prepare_files(options(1465, 24, "L")).is_err());
    assert!(sender.prepare_text(" ", options(1465, 24, "L")).is_err());
    assert!(sender.prepare_text("hello", options(42, 24, "L")).is_err());
    assert!(sender.prepare_text("hello", options(1465, 0, "L")).is_err());
    assert!(
        sender
            .prepare_text("hello", options(1465, 24, "invalid"))
            .is_err()
    );
    assert!(
        sender
            .prepare_text("hello", options(2953, 24, "H"))
            .is_err()
    );
    let info = sender
        .prepare_text("你好 👋", options(1465, 24, "L"))
        .unwrap();
    assert_eq!(info.name, "snippet.txt");
    assert!(sender.next_frame().is_ok());
}

#[test]
fn a_single_nested_file_is_a_zip_and_unsafe_paths_are_refused() {
    let mut sender = SenderState::new();
    sender
        .add_directory_file(
            "notes.txt",
            "text/plain",
            "Trip/day-1/notes.txt",
            b"nested".to_vec(),
        )
        .unwrap();
    let info = sender.prepare_files(options(500, 24, "L")).unwrap();
    assert_eq!(info.name, "transfer.zip");
    assert_eq!(info.file_count, 1);
    assert!(sender.next_frame().is_ok());

    let mut sender = SenderState::new();
    sender
        .add_directory_file("notes.txt", "text/plain", "../notes.txt", b"bad".to_vec())
        .unwrap();
    assert!(sender.prepare_files(options(500, 24, "L")).is_err());
}

#[test]
fn upstreams_wire_is_still_offered_and_an_unknown_one_is_refused() {
    let mut sender = SenderState::new();
    let info = sender
        .prepare_text(
            "hello",
            SendOptions {
                session_id: 0x1234,
                wire: 3,
                ..options(1465, 24, "L")
            },
        )
        .unwrap();
    assert_eq!(info.wire, 3);
    assert_eq!(info.stream_id, 0x1234, "v3 identifies a stream by session");
    assert_eq!(info.cycle_frames, Some(2 * info.source_blocks));

    let mut sender = SenderState::new();
    let error = sender
        .prepare_text(
            "hello",
            SendOptions {
                wire: 5,
                ..options(1465, 24, "L")
            },
        )
        .unwrap_err();
    assert!(error.contains("v4 or v3"), "{error}");
}

#[test]
fn a_colour_code_stacks_three_frames_and_keeps_the_finders_black() {
    let mut sender = SenderState::new();
    let text = "colour ".repeat(4000);
    let info = sender
        .prepare_text(
            &text,
            SendOptions {
                color: true,
                ..options(1465, 24, "L")
            },
        )
        .unwrap();
    assert_eq!(info.layers, 3);
    let cells = info.qr_size * info.qr_size;
    let batch = sender.next_frames(2).unwrap();
    assert_eq!(batch.len(), 2 * cells, "one byte per module per code");
    for code in batch.chunks(cells) {
        assert!(code.iter().all(|&bits| bits <= 0b111));
        // The first dark module in raster order is the top-left finder's
        // corner, dark in every channel and so black.
        assert_eq!(code.iter().find(|&&bits| bits != 0), Some(&0b111));
        // Three different frames make every mixture appear somewhere.
        assert!(code.iter().any(|&bits| bits != 0 && bits != 0b111));
    }
}

#[test]
fn a_selection_over_upstreams_limit_is_kept_for_a_v4_retry() {
    let mut sender = SenderState::new();
    sender
        .add_file("big.bin", "application/octet-stream", vec![0; 65 << 20])
        .unwrap();
    let error = sender
        .prepare_files(SendOptions {
            wire: 3,
            ..options(1465, 24, "L")
        })
        .unwrap_err();
    assert!(error.contains("v3"), "{error}");
    let info = sender.prepare_files(options(2953, 24, "L")).unwrap();
    assert_eq!(info.original_bytes, 65 << 20);
}
