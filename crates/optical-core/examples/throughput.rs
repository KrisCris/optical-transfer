//! Sender and receiver CPU budgets, plus fountain overhead under loss.
//!
//!     cargo run -p optical-core --release --example throughput
//!
//! Three numbers decide whether a setting is achievable on a given machine:
//! how many QR codes per second one core can produce, how fast the decoder
//! absorbs frames, and how many unique frames a lossy receiver needs before
//! the file is whole. The last one is what elimination in the decoder buys.

use std::time::Instant;

use optical_core::{
    fountain::{AddFrame, LtDecoder, LtEncoder},
    protocol::SplitMix32,
    qr::{Ecc, FrameRenderer},
    transfer::{Transfer, TransferSettings},
};

fn main() {
    let payload: Vec<u8> = {
        let mut random = SplitMix32::new(7);
        (0..1 << 20).map(|_| random.next_u32() as u8).collect()
    };

    println!("== sender: frame bytes → QR modules, one core");
    for frame_bytes in [1465, 2331, 2953] {
        let settings = TransferSettings {
            frame_bytes,
            ..TransferSettings::default()
        };
        let mut transfer = Transfer::from_file("bench.bin", "", &payload, settings, 1).unwrap();
        let mut renderer = FrameRenderer::new(Ecc::Low);
        let count = 400;
        let started = Instant::now();
        let mut version = 0;
        for _ in 0..count {
            version = renderer.render(&transfer.next_payload()).unwrap().version;
        }
        let seconds = started.elapsed().as_secs_f64();
        println!(
            "  {frame_bytes:>4} B (V{version:>2}): {:>6.0} codes/s  {:>5.2} ms/code",
            f64::from(count) / seconds,
            seconds * 1000.0 / f64::from(count)
        );
    }

    println!("== receiver: unique frames needed per source block");
    println!("  catch     k  frames/k  redundant  eliminations   frames/s");
    for block_len in [1465 - 23, 2953 - 23] {
        for catch in [1.0, 0.8, 0.6, 0.4, 0.25] {
            let run = simulate(&payload, block_len, catch, 0xbeef);
            println!(
                "  {:>4.0}%  {:>4}  {:>8.3}  {:>9}  {:>12}  {:>9.0}",
                catch * 100.0,
                run.k,
                run.frames as f64 / f64::from(run.k),
                run.redundant,
                run.eliminations,
                run.frames_per_second
            );
        }
    }
}

struct Run {
    k: u16,
    frames: usize,
    redundant: usize,
    eliminations: usize,
    frames_per_second: f64,
}

/// Feed a decoder the carousel as a camera with `catch` probability per frame
/// would see it.
fn simulate(payload: &[u8], block_len: usize, catch: f64, seed: u32) -> Run {
    let encoder = LtEncoder::new(payload, block_len, 0x1234).unwrap();
    let k = encoder.source_blocks();
    let mut decoder = LtDecoder::new(k, block_len, 0x1234, payload.len()).unwrap();
    let mut random = SplitMix32::new(seed);
    let threshold = (catch * f64::from(u32::MAX)) as u32;
    let mut busy = 0.0;
    for seq in 0.. {
        if random.next_u32() > threshold {
            continue;
        }
        let block = encoder.encode(seq);
        let started = Instant::now();
        let outcome = decoder.add_frame(seq, &block).unwrap();
        busy += started.elapsed().as_secs_f64();
        if outcome == AddFrame::Complete {
            assert_eq!(decoder.assemble().unwrap(), payload);
            break;
        }
    }
    Run {
        k,
        frames: decoder.unique_frames(),
        redundant: decoder.redundant_frames(),
        eliminations: decoder.eliminations(),
        frames_per_second: decoder.unique_frames() as f64 / busy.max(1e-9),
    }
}
