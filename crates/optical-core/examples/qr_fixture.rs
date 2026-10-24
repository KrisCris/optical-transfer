//! Emit the sender's actual QR modules for the browser's optical-loop test.
//!
//! This is the seam neither crate's own tests can cross. `optical-core` is
//! pinned against upstream, the receiver is tested against upstream, and the
//! sender is tested against the receiver — but all of that happens in bytes.
//! Whether the *pixels* the sender puts on screen are a QR code a real decoder
//! reads is a separate question, and until a phone is pointed at a real screen
//! the only honest way to ask it is to hand the modules to the same zxing-cpp
//! build the receiver ships and see what comes back.
//!
//!     cargo run -q -p optical-core --example qr_fixture \
//!       > web/app/tests/qr-frames.json

use optical_core::{
    protocol::SplitMix32,
    qr::Ecc,
    transfer::{Transfer, TransferSettings},
};

/// Small codes keep the fixture readable; the geometry is what is under test,
/// not the density.
const FRAME_BYTES: usize = 500;
const SESSION: u16 = 0x6a21;
/// Dropped on the way to the page, exactly as a camera would drop them.
const DROP_EVERY: u32 = 4;
const FRAMES: u32 = 24;

fn main() {
    // Varied rather than repetitive: repeated text gzips to a single block, and
    // a one-block transfer completes on any frame, which would quietly turn the
    // fountain half of this test into nothing.
    let mut random = SplitMix32::new(0x0c71_ca10);
    let mut text = String::from("Optical transfer end to end — 光学传输 ✅\n");
    for line in 0..120 {
        text.push_str(&format!(
            "{line:04} {:08x}{:08x} these bytes left a Rust sender as QR modules\n",
            random.next_u32(),
            random.next_u32()
        ));
    }

    let settings = TransferSettings {
        frame_bytes: FRAME_BYTES,
        ecc: Ecc::Low,
        ..TransferSettings::default()
    };
    let mut transfer = Transfer::from_text(&text, settings, SESSION).expect("text packs");
    let mut renderer = transfer.renderer();

    let mut frames = Vec::new();
    let mut size = 0;
    for seq in 0..FRAMES {
        let payload = transfer.next_payload();
        let frame = renderer.render(&payload).expect("a frame fits its version");
        size = frame.size;
        if seq % DROP_EVERY == DROP_EVERY - 1 {
            continue;
        }
        frames.push(pack_bits(&frame.pixels));
    }

    println!("{{");
    println!("  \"text\": {},", json_string(&text));
    println!("  \"name\": \"snippet.txt\",");
    println!("  \"mediaType\": \"application/vnd.decimen.snippet\",");
    println!("  \"sourceBlocks\": {},", transfer.source_blocks());
    println!("  \"frameBytes\": {FRAME_BYTES},");
    println!("  \"size\": {size},");
    println!("  \"version\": {},", renderer.locked_version().unwrap_or(0));
    println!("  \"quietZone\": {},", optical_core::qr::QUIET_ZONE_MODULES);
    println!("  \"frames\": [");
    for (index, frame) in frames.iter().enumerate() {
        let comma = if index + 1 == frames.len() { "" } else { "," };
        println!("    \"{frame}\"{comma}");
    }
    println!("  ]");
    println!("}}");
}

/// One bit per module, most significant bit first, hex encoded. Row-major, so
/// the browser unpacks it the same way it would read a scanline.
fn pack_bits(pixels: &[bool]) -> String {
    let mut bytes = vec![0_u8; pixels.len().div_ceil(8)];
    for (index, dark) in pixels.iter().enumerate() {
        if *dark {
            bytes[index / 8] |= 0x80 >> (index % 8);
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
