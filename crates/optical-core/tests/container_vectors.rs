use std::io::Write;

use flate2::{Compression as FlateCompression, write::GzEncoder};
use optical_core::{
    MAX_FILE_BYTES,
    container::{Compression, MAX_CONTAINER_BYTES, pack_file, unpack_file, verify_file},
    snippet::{MAX_SNIPPET_BYTES, is_snippet, pack_snippet, snippet_text},
};

#[test]
fn unicode_filename_round_trips() {
    let original = "光学転送".as_bytes();
    let packed = pack_file("../報告_日本語_한글.txt", "text/plain", original).unwrap();
    let file = unpack_file(&packed.container).unwrap();
    assert_eq!(file.name, "報告_日本語_한글.txt");
    assert_eq!(file.media_type, "text/plain");
    assert_eq!(file.bytes, original);
    assert!(verify_file(&file));
}

#[test]
fn changed_payload_fails_sha256() {
    let packed = pack_file("x.bin", "application/octet-stream", &[7; 1024]).unwrap();
    let mut file = unpack_file(&packed.container).unwrap();
    file.bytes[0] ^= 1;
    assert!(!verify_file(&file));
}

#[test]
fn boundaries_and_precompressed_policy_match_upstream() {
    assert!(pack_file("empty", "text/plain", &[]).is_err());

    let maximum = vec![0; MAX_FILE_BYTES];
    assert!(pack_file("maximum.bin", "application/octet-stream", &maximum).is_ok());
    drop(maximum);
    assert!(
        pack_file(
            "too-large.bin",
            "application/octet-stream",
            &vec![0; MAX_FILE_BYTES + 1]
        )
        .is_err()
    );

    // A stored file grows by its header, name and media type. At their
    // longest that is exactly the allowance wire v4 makes, so any file within
    // the limit packs into a container v4 carries.
    let name = "n".repeat(usize::from(u16::MAX));
    let media_type = "m".repeat(usize::from(u16::MAX));
    let longest = pack_file(&name, &media_type, &[1]).unwrap();
    assert_eq!(longest.compression, Compression::None);
    assert_eq!(
        longest.container.len() - 1,
        MAX_CONTAINER_BYTES - MAX_FILE_BYTES
    );
    assert!(pack_file(&format!("{name}n"), &media_type, &[1]).is_err());
    assert!(pack_file(&name, &format!("{media_type}m"), &[1]).is_err());

    let repeated = vec![b'x'; 8192];
    assert_eq!(
        pack_file("notes.txt", "text/plain", &repeated)
            .unwrap()
            .compression,
        Compression::Gzip
    );
    assert_eq!(
        pack_file("photo.jpg", "image/jpeg", &repeated)
            .unwrap()
            .compression,
        Compression::None
    );
}

#[test]
fn malformed_gzip_and_lengths_are_rejected_before_exposure() {
    let packed = pack_file("notes.txt", "text/plain", &vec![b'x'; 4096]).unwrap();
    assert_eq!(packed.compression, Compression::Gzip);

    let mut unknown_compression = packed.container.clone();
    unknown_compression[4] = 2;
    assert!(unpack_file(&unknown_compression).is_err());

    let mut malformed_length = packed.container.clone();
    malformed_length[13..17].copy_from_slice(&1_u32.to_le_bytes());
    assert!(unpack_file(&malformed_length).is_err());

    let mut trailer_mismatch = packed.container.clone();
    let last = trailer_mismatch.len() - 4;
    trailer_mismatch[last..].copy_from_slice(&1_u32.to_le_bytes());
    assert!(unpack_file(&trailer_mismatch).is_err());

    let mut truncated = packed.container;
    let payload_length = u32::from_le_bytes(truncated[13..17].try_into().unwrap());
    truncated.pop();
    truncated[13..17].copy_from_slice(&(payload_length - 1).to_le_bytes());
    assert!(unpack_file(&truncated).is_err());
}

#[test]
fn decompression_stops_before_exceeding_declared_output_limit() {
    let mut encoder = GzEncoder::new(Vec::new(), FlateCompression::default());
    encoder.write_all(&vec![b'z'; 1025]).unwrap();
    let mut compressed = encoder.finish().unwrap();
    let end = compressed.len() - 4;
    compressed[end..].copy_from_slice(&1024_u32.to_le_bytes());

    let container = raw_gzip_container(&compressed, 1024);
    assert!(unpack_file(&container).is_err());
}

#[test]
fn snippets_use_pinned_utf8_limit_and_media_type() {
    assert_eq!(MAX_SNIPPET_BYTES, 4 * 1024 * 1024);
    assert!(pack_snippet("  \n\t").is_err());
    let packed = pack_snippet("鍵\nsecond line").unwrap();
    let file = unpack_file(&packed.container).unwrap();
    assert!(is_snippet(&file));
    assert_eq!(snippet_text(&file).unwrap(), "鍵\nsecond line");
    assert!(pack_snippet(&"あ".repeat(MAX_SNIPPET_BYTES / 3 + 1)).is_err());
}

fn raw_gzip_container(payload: &[u8], original_size: u32) -> Vec<u8> {
    let name = b"x";
    let media_type = b"application/octet-stream";
    let mut container = vec![0; 49 + name.len() + media_type.len() + payload.len()];
    container[..4].copy_from_slice(b"DCF2");
    container[4] = 1;
    container[5..7].copy_from_slice(&(name.len() as u16).to_le_bytes());
    container[7..9].copy_from_slice(&(media_type.len() as u16).to_le_bytes());
    container[9..13].copy_from_slice(&original_size.to_le_bytes());
    container[13..17].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    container[49..50].copy_from_slice(name);
    container[50..50 + media_type.len()].copy_from_slice(media_type);
    container[50 + media_type.len()..].copy_from_slice(payload);
    container
}
