use optical_wasm::SenderState;

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
    let info = sender.prepare_files(1465, 24, "L", 42).unwrap();
    assert_eq!(info.name, "hello.txt");
    assert_eq!(info.original_bytes, 22);
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
    let info = sender.prepare_files(500, 15, "L", 43).unwrap();
    assert_eq!(info.name, "transfer.zip");
    assert!(info.original_bytes > 5);
    assert_eq!(info.file_count, 2);
    assert!(sender.next_frame().is_ok());
}

#[test]
fn text_and_invalid_settings_are_handled_before_streaming() {
    let mut sender = SenderState::new();
    assert!(sender.next_frame().is_err());
    assert!(sender.prepare_files(1465, 24, "L", 1).is_err());
    assert!(sender.prepare_text(" ", 1465, 24, "L", 1).is_err());
    assert!(sender.prepare_text("hello", 42, 24, "L", 1).is_err());
    assert!(sender.prepare_text("hello", 1465, 0, "L", 1).is_err());
    assert!(
        sender
            .prepare_text("hello", 1465, 24, "invalid", 1)
            .is_err()
    );
    assert!(sender.prepare_text("hello", 2953, 24, "H", 1).is_err());
    let info = sender.prepare_text("你好 👋", 1465, 24, "L", 1).unwrap();
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
    let info = sender.prepare_files(500, 24, "L", 44).unwrap();
    assert_eq!(info.name, "transfer.zip");
    assert_eq!(info.file_count, 1);
    assert!(sender.next_frame().is_ok());

    let mut sender = SenderState::new();
    sender
        .add_directory_file("notes.txt", "text/plain", "../notes.txt", b"bad".to_vec())
        .unwrap();
    assert!(sender.prepare_files(500, 24, "L", 44).is_err());
}
