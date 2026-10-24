//! Emit a deterministic ZIP to stdout for extraction with an independent reader.
use std::io::{self, Write};

use optical_core::bundle::{SelectedFile, bundle_files};

fn main() {
    let files = [
        ("photos/旅行/照片.jpg", vec![0, 1, 255]),
        ("photos/empty.txt", vec![]),
        ("hello.txt", b"123456789".to_vec()),
        ("HELLO.txt", b"second".to_vec()),
        ("hello (2).txt", b"existing suffix".to_vec()),
    ]
    .into_iter()
    .map(|(name, bytes)| SelectedFile {
        name: name.into(),
        relative_path: name.contains('/').then(|| name.into()),
        media_type: "application/octet-stream".into(),
        bytes,
    })
    .collect();
    let archive = bundle_files(files).expect("valid sample files");
    io::stdout()
        .lock()
        .write_all(&archive.bytes)
        .expect("write sample ZIP");
}
