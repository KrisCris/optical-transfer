use optical_core::{
    MAX_FILE_BYTES,
    bundle::{MAX_BUNDLE_FILES, SelectedFile, bundle_files},
};

fn file(name: &str, bytes: &[u8]) -> SelectedFile {
    SelectedFile {
        name: name.into(),
        relative_path: None,
        media_type: "image/jpeg".into(),
        bytes: bytes.into(),
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> usize {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize
}
fn u32_at(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

// Read the stored entries independently from their local and central headers.
fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let end = bytes.len() - 22;
    assert_eq!(&bytes[end..end + 4], b"PK\x05\x06");
    let count = u16_at(bytes, end + 10);
    assert_eq!(u16_at(bytes, end + 8), count);
    let central_start = u32_at(bytes, end + 16);
    assert_eq!(central_start + u32_at(bytes, end + 12), end);
    let mut central = central_start;
    let mut next_local = 0;
    let mut result = Vec::new();
    for _ in 0..count {
        assert_eq!(&bytes[central..central + 4], b"PK\x01\x02");
        let local = u32_at(bytes, central + 42);
        assert_eq!(local, next_local);
        assert_eq!(&bytes[local..local + 4], b"PK\x03\x04");
        assert_eq!(u16_at(bytes, local + 6), 1 << 11);
        assert_eq!(u16_at(bytes, local + 8), 0);
        assert_eq!(u16_at(bytes, local + 10), 0);
        assert_eq!(u16_at(bytes, local + 12), 33); // 1980-01-01
        assert_eq!(
            &bytes[local + 6..local + 26],
            &bytes[central + 8..central + 28]
        );
        let size = u32_at(bytes, local + 18);
        assert_eq!(size, u32_at(bytes, local + 22));
        let name_len = u16_at(bytes, local + 26);
        assert_eq!(name_len, u16_at(bytes, central + 28));
        assert_eq!(u16_at(bytes, local + 28), 0);
        assert_eq!(
            &bytes[local + 30..local + 30 + name_len],
            &bytes[central + 46..central + 46 + name_len]
        );
        let name = String::from_utf8(bytes[local + 30..local + 30 + name_len].to_vec()).unwrap();
        let data = bytes[local + 30 + name_len..local + 30 + name_len + size].to_vec();
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        assert_eq!(u32_at(bytes, local + 14), crc.sum() as usize);
        next_local = local + 30 + name_len + size;
        central += 46 + name_len;
        result.push((name, data));
    }
    assert_eq!(next_local, central_start);
    assert_eq!(central, end);
    result
}

#[test]
fn single_file_keeps_type_and_bytes_without_archive() {
    let selected = file("../camera/照片.jpg", &[0xff, 0xd8, 0, 0xff]);
    let original_ptr = selected.bytes.as_ptr();
    let result = bundle_files(vec![selected]).unwrap();
    assert_eq!(result.name, "照片.jpg");
    assert_eq!(result.media_type, "image/jpeg");
    assert_eq!(result.bytes, [0xff, 0xd8, 0, 0xff]);
    assert_eq!(result.bytes.as_ptr(), original_ptr);
}

#[test]
fn multi_file_zip_preserves_empty_binary_and_unicode_contents() {
    let result = bundle_files(vec![
        file("照片.jpg", &[0, 1, 255]),
        file("empty.txt", b""),
        file("hello.txt", b"123456789"),
    ])
    .unwrap();
    assert_eq!(result.name, "transfer.zip");
    assert_eq!(result.media_type, "application/zip");
    assert_eq!(
        entries(&result.bytes),
        vec![
            ("照片.jpg".into(), vec![0, 1, 255]),
            ("empty.txt".into(), vec![]),
            ("hello.txt".into(), b"123456789".to_vec())
        ]
    );
}

#[test]
fn paths_windows_names_and_case_collisions_are_safe() {
    let names = [
        "../photo.jpg",
        "C:\\camera\\PHOTO.jpg",
        "photo (2).jpg",
        "photo.jpg",
        "../..",
        "...",
        "CON.txt",
        "NUL",
        "a:b?*.txt",
        "trailing. ",
        "É.jpg",
        "é.jpg",
    ];
    let files = names
        .iter()
        .enumerate()
        .map(|(index, name)| file(name, &[index as u8]))
        .collect();
    let result = entries(&bundle_files(files).unwrap().bytes);
    let actual: Vec<_> = result.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        actual,
        [
            "photo.jpg",
            "PHOTO (3).jpg",
            "photo (2).jpg",
            "photo (4).jpg",
            "transfer.bin",
            "transfer (2).bin",
            "_CON.txt",
            "_NUL",
            "a_b__.txt",
            "trailing",
            "É.jpg",
            "é (2).jpg"
        ]
    );
    for (index, (_, data)) in result.iter().enumerate() {
        assert_eq!(data, &[index as u8]);
    }
    assert_eq!(
        bundle_files(vec![file("CON.txt", b"x")]).unwrap().name,
        "_CON.txt"
    );
}

#[test]
fn rejects_empty_selection_too_many_files_and_long_names() {
    assert!(bundle_files(vec![]).is_err());
    assert!(bundle_files((0..=MAX_BUNDLE_FILES).map(|_| file("a", b"")).collect()).is_err());
    assert_eq!(
        entries(
            &bundle_files((0..MAX_BUNDLE_FILES).map(|_| file("a", b"")).collect())
                .unwrap()
                .bytes
        )
        .len(),
        MAX_BUNDLE_FILES
    );
    assert!(bundle_files(vec![file(&"a".repeat(65_536), b"x")]).is_err());
    assert!(
        bundle_files(vec![
            file(&"a".repeat(65_535), b"x"),
            file(&"a".repeat(65_535), b"x")
        ])
        .is_err()
    );
}

#[test]
fn archive_limit_includes_headers_and_single_file_limit_is_checked() {
    let large = SelectedFile {
        name: "a".into(),
        relative_path: None,
        media_type: "".into(),
        bytes: vec![0; MAX_FILE_BYTES],
    };
    assert!(bundle_files(vec![large, file("b", b"")]).is_err());
    let large = SelectedFile {
        name: "a".into(),
        relative_path: None,
        media_type: "".into(),
        bytes: vec![0; MAX_FILE_BYTES + 1],
    };
    assert!(bundle_files(vec![large]).is_err());
}

fn folder_file(path: &str, bytes: &[u8]) -> SelectedFile {
    SelectedFile {
        relative_path: Some(path.into()),
        ..file(path.rsplit('/').next().unwrap(), bytes)
    }
}

#[test]
fn single_folder_file_is_zipped_with_root_and_nested_directories() {
    let result = bundle_files(vec![folder_file("photos/旅行/day.1/empty.txt", b"")]).unwrap();
    assert_eq!(result.name, "transfer.zip");
    assert_eq!(result.media_type, "application/zip");
    assert!(result.relative_path.is_none());
    assert_eq!(
        entries(&result.bytes),
        [("photos/旅行/day.1/empty.txt".into(), vec![])]
    );
}

#[test]
fn separate_folders_and_regular_files_keep_their_own_basenames() {
    let result = bundle_files(vec![
        folder_file("photos/one/photo.jpg", b"one"),
        folder_file("photos/two/photo.jpg", b"two"),
        file("photo.jpg", b"regular"),
    ])
    .unwrap();
    assert_eq!(
        entries(&result.bytes),
        [
            ("photos/one/photo.jpg".into(), b"one".to_vec()),
            ("photos/two/photo.jpg".into(), b"two".to_vec()),
            ("photo.jpg".into(), b"regular".to_vec())
        ]
    );
}

#[test]
fn duplicate_leaf_suffixes_do_not_change_dotted_directory_names() {
    let result = bundle_files(vec![
        folder_file("root.v1/files/photo", b"one"),
        folder_file("root.v1/files/photo", b"two"),
        folder_file("root.v1/files/photo (2)", b"three"),
    ])
    .unwrap();
    assert_eq!(
        entries(&result.bytes),
        [
            ("root.v1/files/photo".into(), b"one".to_vec()),
            ("root.v1/files/photo (3)".into(), b"two".to_vec()),
            ("root.v1/files/photo (2)".into(), b"three".to_vec())
        ]
    );
}

#[test]
fn folder_components_are_sanitized_without_losing_the_hierarchy() {
    let result = bundle_files(vec![folder_file("root?/CON/NUL.txt", b"data")]).unwrap();
    assert_eq!(result.media_type, "application/zip");
    assert_eq!(
        entries(&result.bytes),
        [("root_/_CON/_NUL.txt".into(), b"data".to_vec())]
    );
}

#[test]
fn rejects_absolute_traversal_empty_and_windows_drive_paths() {
    for path in [
        "",
        "/root/a",
        "\\root\\a",
        "root\\a",
        "root//a",
        "root/a/",
        "./root/a",
        "root/./a",
        "root/../a",
        "../a",
        "C:/root/a",
        "C:relative.txt",
        "root/C:/a",
    ] {
        assert!(
            bundle_files(vec![folder_file(path, b"x")]).is_err(),
            "accepted hostile path: {path:?}"
        );
    }
}

#[test]
fn rejects_directory_aliases_and_file_directory_conflicts_in_either_order() {
    for paths in [
        ["root?/one", "root*/two"],
        ["root/dir/one", "root/DIR/two"],
        ["root/dir/one", "ROOT/other/two"],
        ["root/dir/one", "root/dir. /two"],
        ["root/node", "root/node/child"],
        ["root/NODE", "root/node/child"],
    ] {
        for pair in [paths, [paths[1], paths[0]]] {
            assert!(
                bundle_files(pair.iter().map(|path| folder_file(path, b"x")).collect()).is_err(),
                "accepted conflicting paths: {pair:?}"
            );
        }
    }
    for files in [
        vec![file("root", b"x"), folder_file("root/child", b"y")],
        vec![folder_file("root/child", b"y"), file("ROOT", b"x")],
    ] {
        assert!(bundle_files(files).is_err());
    }
    // A generated duplicate name must not become a directory that already exists.
    assert!(
        bundle_files(vec![
            folder_file("root/item", b"1"),
            folder_file("root/item", b"2"),
            folder_file("root/item (2)/child", b"3")
        ])
        .is_err()
    );
}
