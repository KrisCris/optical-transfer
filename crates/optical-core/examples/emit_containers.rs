//! Emit Rust-packed containers for the upstream oracle to read back.
//!
//! The mirror of `tests/upstream_conformance.rs`: that test proves this build
//! can read what a browser sends, this emitter feeds the other direction, which
//! is the one the desktop sender depends on. See
//! `scripts/upstream-conformance/README.md`.
//!
//!     cargo run -q -p optical-core --example emit_containers > containers.txt

use optical_core::{container::pack_file, protocol::SplitMix32, snippet::pack_snippet};

/// One vector: the identifier, the name and media type a receiver should
/// report, the original bytes, and the container carrying them.
struct Record {
    id: &'static str,
    name: &'static str,
    media_type: &'static str,
    original: Vec<u8>,
    container: Vec<u8>,
}

fn main() {
    let mut records = Vec::new();

    let tiny = "hello optical — 光学 ✅".as_bytes().to_vec();
    let packed = pack_file("note.txt", "text/plain", &tiny).unwrap();
    records.push(Record {
        id: "tiny-none",
        name: "note.txt",
        media_type: "text/plain",
        original: tiny,
        container: packed.container,
    });

    let repeated = "the same sentence over and over. ".repeat(400).into_bytes();
    let packed = pack_file("repeat.txt", "text/plain", &repeated).unwrap();
    records.push(Record {
        id: "gzip",
        name: "repeat.txt",
        media_type: "text/plain",
        original: repeated,
        container: packed.container,
    });

    let mut random = SplitMix32::new(12345);
    let noise: Vec<u8> = (0..4096).map(|_| random.next_u32() as u8).collect();
    let packed = pack_file("noise.bin", "application/octet-stream", &noise).unwrap();
    records.push(Record {
        id: "incompressible",
        name: "noise.bin",
        media_type: "application/octet-stream",
        original: noise,
        container: packed.container,
    });

    // A name a receiver must not be able to turn into a path, and a media type
    // left empty so both sides have to reach the same default.
    let payload = b"basename only".to_vec();
    let packed = pack_file("../../etc/passwd\u{7f}.txt", "", &payload).unwrap();
    records.push(Record {
        id: "hostile-name",
        name: "passwd.txt",
        media_type: "application/octet-stream",
        original: payload,
        container: packed.container,
    });

    let text = "snippet across the optical channel — 光学传输\n".repeat(60);
    let packed = pack_snippet(&text).unwrap();
    records.push(Record {
        id: "snippet",
        name: "snippet.txt",
        media_type: "application/vnd.decimen.snippet",
        original: text.into_bytes(),
        container: packed.container,
    });

    for record in records {
        println!("case {}", record.id);
        println!("name {}", record.name);
        println!("type {}", record.media_type);
        println!("original {}", hex(&record.original));
        println!("container {}", hex(&record.container));
        println!("end");
        println!();
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
