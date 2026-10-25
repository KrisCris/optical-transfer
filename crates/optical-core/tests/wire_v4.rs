//! Wire v4: the RaptorQ layout, coder and receive path.
//!
//! v4 is this repository's own format, so there is no upstream oracle to pin
//! it against. What these tests pin instead is everything a receiver relies
//! on without being told: the block layout both ends derive, the frame to
//! symbol mapping, decoding from (nearly) any `K` symbols whatever the loss
//! pattern, and the refusals that keep a hostile header from claiming memory.

use optical_core::{
    MAX_FILE_BYTES, OpticalError,
    container::MAX_CONTAINER_BYTES,
    fountain::AddFrame,
    protocol::{FrameHeaderV4, MAX_SYMBOL_ID, SplitMix32, fnv1a, pack_frame_v4, parse_frame_v4},
    raptor::{Layout, MAX_BLOCK_SYMBOLS, MAX_SYMBOLS, RaptorDecoder, RaptorEncoder},
    receiver::{ReceiverEvent, ReceiverState},
    transfer::{Transfer, TransferSettings},
};

fn random_bytes(len: usize, seed: u32) -> Vec<u8> {
    let mut random = SplitMix32::new(seed);
    (0..len).map(|_| random.next_u32() as u8).collect()
}

/// Feed `encoder`'s carousel from `start` into a fresh decoder, keeping each
/// frame with probability `catch`. Returns the payload and the unique frames
/// it took.
fn run(encoder: &RaptorEncoder, start: u32, catch: f64, seed: u32) -> (Vec<u8>, usize) {
    let layout = encoder.layout();
    let mut decoder = RaptorDecoder::new(layout.total_len(), layout.symbol_len()).unwrap();
    let mut random = SplitMix32::new(seed);
    let threshold = (catch * f64::from(u32::MAX)) as u32;
    let limit = start + (f64::from(layout.symbols()) * 2.0 / catch) as u32 + 1000;
    for seq in start..limit {
        if random.next_u32() > threshold {
            continue;
        }
        let (block, id, symbol) = encoder.encode(seq);
        if decoder.add_symbol(block, id, &symbol).unwrap() == AddFrame::Complete {
            return (decoder.take_payload().unwrap(), decoder.unique_frames());
        }
    }
    panic!("the stream did not complete within {limit} frames");
}

#[test]
fn the_layout_splits_into_blocks_of_at_most_the_ceiling() {
    let small = Layout::new(1000, 100).unwrap();
    assert_eq!((small.symbols(), small.blocks()), (10, 1));
    assert_eq!(small.block_symbols(0), 10);

    // One symbol over the ceiling makes two blocks, as equal as RFC 6330's
    // partition makes them.
    let symbols = MAX_BLOCK_SYMBOLS + 1;
    let split = Layout::new(symbols as usize * 4, 4).unwrap();
    assert_eq!(split.blocks(), 2);
    assert_eq!(split.block_symbols(0) + split.block_symbols(1), symbols);
    assert!(split.block_symbols(0) - split.block_symbols(1) <= 1);

    // `locate`, `block_start`, `position` and `sequence` agree with each other
    // across uneven blocks.
    let uneven = Layout::new(3 * MAX_BLOCK_SYMBOLS as usize * 8 + 5 * 8 + 3, 8).unwrap();
    assert_eq!(uneven.blocks(), 4);
    let mut index = 0;
    for block in 0..uneven.blocks() {
        assert_eq!(uneven.block_start(block), index);
        for id in [0, uneven.block_symbols(block) - 1] {
            assert_eq!(uneven.locate(uneven.block_start(block) + id), (block, id));
        }
        index += uneven.block_symbols(block);
    }
    assert_eq!(index, uneven.symbols());
    for seq in [0, 1, 3, 4, 5, 1_000_003] {
        let (block, id) = uneven.position(seq);
        assert_eq!(uneven.sequence(block, id), u64::from(seq));
    }
    // Symbol ids are 24 bits; the carousel wraps them rather than overflowing.
    let (_, id) = small.position(u32::MAX);
    assert!(id <= MAX_SYMBOL_ID);
}

#[test]
fn the_layout_refuses_dimensions_no_honest_sender_produces() {
    for (total_len, symbol_len) in [
        (0, 100),
        (100, 0),
        (MAX_CONTAINER_BYTES + 1, 2937),
        (1000, usize::from(u16::MAX) + 1),
        // Millions of tiny symbols: tens of megabytes of bookkeeping for a
        // header to claim.
        (MAX_SYMBOLS as usize + 1, 1),
    ] {
        assert_eq!(
            Layout::new(total_len, symbol_len).err(),
            Some(OpticalError::UnsupportedDimensions),
            "{total_len} bytes in {symbol_len} byte symbols"
        );
        assert!(RaptorDecoder::new(total_len, symbol_len).is_err());
    }
    // The largest container a permitted file packs into, at the smallest
    // offered frame size: within the symbol budget too.
    assert!(Layout::new(MAX_CONTAINER_BYTES, 484).is_ok());
}

#[test]
fn a_clean_sweep_needs_no_decoding_and_no_extra_frames() {
    let payload = random_bytes(200_000, 1);
    let encoder = RaptorEncoder::new(&payload, 2937).unwrap();
    let (decoded, frames) = run(&encoder, 0, 1.0, 1);
    assert_eq!(decoded, payload);
    assert_eq!(frames, encoder.layout().symbols() as usize);
}

#[test]
fn lossy_streams_decode_from_about_k_symbols_at_any_loss_rate() {
    // The headline property, and the reason for the change: wire v3's LT code
    // needs 1.1 to 1.7 frames per block here, depending on the loss.
    let payload = random_bytes(1_048_576, 2);
    let encoder = RaptorEncoder::new(&payload, 2937).unwrap();
    let k = encoder.layout().symbols() as usize;
    for (catch, seed) in [(0.95, 3), (0.8, 4), (0.5, 5), (0.3, 6), (0.1, 7)] {
        let (decoded, frames) = run(&encoder, 0, catch, seed);
        assert_eq!(decoded, payload, "catch {catch}");
        assert!(
            frames <= k + 3,
            "catch {catch}: {frames} frames for {k} symbols"
        );
    }
}

#[test]
fn a_receiver_that_misses_the_whole_sweep_decodes_from_repair_symbols_alone() {
    let payload = random_bytes(300_000, 8);
    let encoder = RaptorEncoder::new(&payload, 1465).unwrap();
    let k = encoder.layout().symbols();
    let (decoded, frames) = run(&encoder, 3 * k, 0.6, 9);
    assert_eq!(decoded, payload);
    assert!(frames <= k as usize + 3);
}

#[test]
fn several_source_blocks_interleave_and_all_decode() {
    // Small symbols keep this quick while still needing three blocks.
    let payload = random_bytes(40_000 * 32 - 7, 10);
    let encoder = RaptorEncoder::new(&payload, 32).unwrap();
    let layout = encoder.layout();
    assert_eq!(layout.blocks(), 3);
    // Consecutive frames visit every block before any block's next symbol.
    let blocks: Vec<u8> = (0..6).map(|seq| encoder.encode(seq).0).collect();
    assert_eq!(blocks, [0, 1, 2, 0, 1, 2]);
    let (decoded, frames) = run(&encoder, 0, 0.7, 11);
    assert_eq!(decoded, payload);
    // Each block must reach its own K, so blocks cost a little each.
    assert!(
        frames as f64 <= f64::from(layout.symbols()) * 1.03,
        "{frames} frames for {} symbols",
        layout.symbols()
    );
}

#[test]
fn duplicates_redundant_frames_and_bad_symbols_are_told_apart() {
    let payload = random_bytes(10_000, 12);
    let encoder = RaptorEncoder::new(&payload, 1000).unwrap();
    let mut decoder = RaptorDecoder::new(payload.len(), 1000).unwrap();
    let (block, id, symbol) = encoder.encode(0);
    assert_eq!(
        decoder.add_symbol(block, id, &symbol),
        Ok(AddFrame::Accepted)
    );
    assert_eq!(
        decoder.add_symbol(block, id, &symbol),
        Ok(AddFrame::Duplicate)
    );
    assert!(decoder.is_solved(0) && !decoder.is_solved(1));
    assert_eq!(decoder.solved_map()[..2], [1, 0]);

    // A frame whose length or block number does not fit the layout is
    // refused rather than handed to the coder.
    assert_eq!(
        decoder.add_symbol(0, 5, &symbol[..999]),
        Err(OpticalError::InvalidFrame)
    );
    assert_eq!(
        decoder.add_symbol(1, 5, &symbol),
        Err(OpticalError::InvalidFrame)
    );
    // The largest symbol id is a symbol like any other.
    let far = encoder.symbol(0, MAX_SYMBOL_ID);
    assert_eq!(
        decoder.add_symbol(0, MAX_SYMBOL_ID, &far),
        Ok(AddFrame::Accepted)
    );
    assert_eq!(decoder.pending_symbols(), 1);

    let mut seq = 1;
    while !decoder.is_complete() {
        let (block, id, symbol) = encoder.encode(seq);
        decoder.add_symbol(block, id, &symbol).unwrap();
        seq += 1;
    }
    assert_eq!(decoder.solved_count(), 10);
    assert!(decoder.solved_map().iter().all(|known| *known == 1));
    assert!(!decoder.is_exhausted());
    let (block, id, symbol) = encoder.encode(seq + 50);
    assert_eq!(
        decoder.add_symbol(block, id, &symbol),
        Ok(AddFrame::Complete)
    );
    assert_eq!(decoder.take_payload().unwrap(), payload);
}

/// A v4 file stream through the receiver both apps run.
fn receive(transfer: &mut Transfer, drop_every: u32, limit: u32) -> ReceiverEvent {
    let mut receiver = ReceiverState::new();
    for index in 0..limit {
        let frame = transfer.next_payload();
        if drop_every != 0 && index % drop_every == drop_every - 1 {
            continue;
        }
        match receiver.add_frame(&frame) {
            ReceiverEvent::Progress { stats, .. } => assert_eq!(stats.wire, 4),
            event => return event,
        }
    }
    panic!("no result within {limit} frames");
}

#[test]
fn the_receiver_completes_a_v4_file_and_reports_its_dimensions() {
    let payload = random_bytes(500_000, 13);
    let mut transfer = Transfer::from_file(
        "noise.bin",
        "application/octet-stream",
        &payload,
        TransferSettings {
            frame_bytes: 2953,
            ..TransferSettings::default()
        },
        1,
    )
    .unwrap();
    let ReceiverEvent::Complete {
        name, bytes, stats, ..
    } = receive(&mut transfer, 3, 10_000)
    else {
        panic!("the transfer did not complete");
    };
    assert_eq!(name, "noise.bin");
    assert_eq!(bytes, payload);
    assert_eq!(stats.wire, 4);
    // Balanced under the 2937-byte ceiling a 2953-byte frame leaves.
    assert_eq!(stats.block_len, transfer.block_len());
    assert!(stats.block_len <= 2953 - 16 && stats.block_len > 2900);
    assert_eq!(stats.total_blocks, transfer.source_blocks() as usize);
    assert_eq!(stats.solved_blocks, stats.total_blocks);
    // Two of every three frames arrived, and no more of them were needed
    // than the payload has symbols.
    assert!(stats.unique_frames <= stats.total_blocks + 3);
    assert!(f64::from(stats.seq_span) >= stats.unique_frames as f64 * 1.45);
}

#[test]
fn a_v4_stream_whose_container_does_not_match_its_checksum_is_refused() {
    // A frame that decodes but describes a different container: the symbols
    // are honest, the checksum is not. Nothing may reach the caller.
    let payload = random_bytes(5000, 14);
    let mut transfer =
        Transfer::from_file("a.bin", "", &payload, TransferSettings::default(), 1).unwrap();
    let mut receiver = ReceiverState::new();
    let mut outcome = None;
    for _ in 0..50 {
        let frame = transfer.next_payload();
        let parsed = parse_frame_v4(&frame).unwrap();
        let lie = pack_frame_v4(
            FrameHeaderV4 {
                checksum: parsed.header.checksum ^ 1,
                ..parsed.header
            },
            parsed.symbol,
        )
        .unwrap();
        match receiver.add_frame(&lie) {
            ReceiverEvent::Progress { .. } => {}
            event => {
                outcome = Some(event);
                break;
            }
        }
    }
    let Some(ReceiverEvent::Failed { message }) = outcome else {
        panic!("a mismatched checksum must fail the transfer, got {outcome:?}");
    };
    assert!(message.contains("checksum"), "{message}");
}

#[test]
fn a_receiver_follows_a_sender_that_changes_wire_or_symbol_size() {
    // Incompressible, so the stream takes more than one frame.
    let text: String = random_bytes(8000, 15)
        .into_iter()
        .map(|byte| char::from(b'!' + byte % 90))
        .collect();
    let mut v4 = Transfer::from_text(&text, TransferSettings::default(), 1).unwrap();
    let mut v3 = Transfer::from_text(
        &text,
        TransferSettings {
            wire: optical_core::protocol::Wire::V3,
            ..TransferSettings::default()
        },
        2,
    )
    .unwrap();
    let mut smaller = Transfer::from_text(
        &text,
        TransferSettings {
            frame_bytes: 1000,
            ..TransferSettings::default()
        },
        3,
    )
    .unwrap();

    let mut receiver = ReceiverState::new();
    let started = |event: ReceiverEvent| match event {
        ReceiverEvent::Progress { new_stream, .. } => new_stream,
        other => panic!("unexpected {other:?}"),
    };
    assert!(started(receiver.add_frame(&v4.next_payload())));
    assert!(!started(receiver.add_frame(&v4.next_payload())));
    assert!(started(receiver.add_frame(&v3.next_payload())));
    assert!(started(receiver.add_frame(&smaller.next_payload())));
    // ...and finishes whichever it ends up watching.
    loop {
        match receiver.add_frame(&smaller.next_payload()) {
            ReceiverEvent::Progress { .. } => {}
            ReceiverEvent::Complete { bytes, .. } => {
                assert_eq!(String::from_utf8(bytes).unwrap(), text);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn a_hostile_v4_header_is_refused_before_anything_is_allocated() {
    // 128 MiB claimed in one-byte symbols: within the file limit, far beyond
    // anything a sender emits.
    let frame = pack_frame_v4(
        FrameHeaderV4 {
            flags: 0,
            checksum: fnv1a(b"lie"),
            total_len: MAX_FILE_BYTES as u32,
            block: 0,
            symbol: 0,
        },
        &[0],
    )
    .unwrap();
    let mut receiver = ReceiverState::new();
    let ReceiverEvent::Incompatible { message } = receiver.add_frame(&frame) else {
        panic!("a header like that must be refused");
    };
    assert!(message.contains("will not decode"), "{message}");
    assert!(receiver.solved_map().is_empty());
}
