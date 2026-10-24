//! Emit the frame fixture the browser smoke test replays.
//!
//! The point of the fixture is that it is produced by the same `optical-core`
//! the WASM module contains, but *outside* the browser: the page proves the
//! compiled module decodes bytes it never generated, which is the only version
//! of "native and browser agree" worth asserting.
//!
//!     cargo run -p optical-wasm --example frame_fixture \
//!       > web/app/tests/frames.json

use optical_core::{
    fountain::LtEncoder,
    protocol::{FrameHeader, fnv1a, pack_frame},
    snippet::pack_snippet,
};

/// Small on purpose, so the fixture stays readable and the browser has to run
/// a real multi-block fountain decode rather than a single systematic frame.
const BLOCK_LEN: usize = 96;
const SESSION: u16 = 0x2a3f;
/// Dropped on the way to the page, exactly as a camera would drop them.
const DROP_EVERY: u32 = 5;

fn main() {
    let text = "Decimen optical transfer — 光学传输 ✅\n\
                The receiver never sees these bytes until the frame checksum \
                and the container SHA-256 both agree.\n";
    let packed = pack_snippet(text).unwrap();
    let encoder = LtEncoder::new(&packed.container, BLOCK_LEN, SESSION).unwrap();
    let header = FrameHeader {
        session_id: SESSION,
        seq: 0,
        source_blocks: encoder.source_blocks(),
        block_len: BLOCK_LEN as u16,
        total_len: packed.container.len() as u32,
        payload_fnv: fnv1a(&packed.container),
        flags: 0,
    };

    let frames: Vec<String> = (0..u32::from(encoder.source_blocks()) * 4)
        .filter(|seq| seq % DROP_EVERY != DROP_EVERY - 1)
        .map(|seq| {
            pack_frame(FrameHeader { seq, ..header }, &encoder.encode(seq))
                .unwrap()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        })
        .collect();

    println!("{{");
    println!("  \"text\": {},", json_string(text));
    println!("  \"name\": \"snippet.txt\",");
    println!("  \"mediaType\": \"application/vnd.decimen.snippet\",");
    println!("  \"sourceBlocks\": {},", encoder.source_blocks());
    println!("  \"blockLen\": {BLOCK_LEN},");
    println!("  \"frames\": [");
    for (index, frame) in frames.iter().enumerate() {
        let comma = if index + 1 == frames.len() { "" } else { "," };
        println!("    \"{frame}\"{comma}");
    }
    println!("  ]");
    println!("}}");
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            control if control < ' ' => out.push_str(&format!("\\u{:04x}", control as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}
