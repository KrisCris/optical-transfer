use optical_core::{
    fountain::{
        AddFrame, LtDecoder, LtEncoder, cycle_length, dlog, frame_composition, frame_indices,
        soliton_cdf,
    },
    protocol::{FrameHeader, SplitMix32, fnv1a, pack_frame, parse_frame},
};

#[test]
fn sweep_is_systematic_and_cycles() {
    for k in [1_u16, 2, 17, 179] {
        for position in 0..k {
            assert_eq!(frame_composition(k, 9, u32::from(position)), vec![position]);
            assert_eq!(
                frame_composition(k, 9, u32::from(position) + 6 * cycle_length(k)),
                vec![position]
            );
        }
    }
}

#[test]
fn encoded_stream_fingerprints_match_upstream() {
    let cases = [
        (1, 64, 1, 0xf6a1_15c5),
        (23, 64, 7, 0x4a5d_3eaa),
        (179, 2933, 4242, 0x54f7_8d05),
        (716, 1445, 65535, 0x75b7_3b85),
    ];
    for (k, block_len, session, expected) in cases {
        let payload = test_payload(k * block_len - 7);
        let encoder = LtEncoder::new(&payload, block_len, session).unwrap();
        let stream: Vec<u8> = (0..64).flat_map(|seq| encoder.encode(seq)).collect();
        assert_eq!(fnv1a(&stream), expected);
    }
}

#[test]
#[allow(clippy::approx_constant)]
fn legacy_deterministic_vectors_match_upstream() {
    for (value, expected) in [
        (1.0, 0.0),
        (1.5, 0.4054651081081644),
        (2.0, 0.6931471805599453),
        (2.718281828459045, 1.0),
        (10.0, 2.3025850929940455),
        (20.0, 2.995732273553991),
        (200.0, 5.298317366548036),
        (2000.0, 7.600902459542082),
        (2986.0, 8.001689978099137),
        (44000.0, 10.691944912900398),
        (131070.0, 11.78348681061359),
    ] {
        assert_eq!(dlog(value), expected);
    }

    let mut dlog_bytes = Vec::new();
    for k in 1_u16..=65535 {
        dlog_bytes.extend_from_slice(&dlog(f64::from(u32::from(k) * 2)).to_le_bytes());
    }
    for i in 64..(64 * 4096) {
        dlog_bytes.extend_from_slice(&dlog(f64::from(i) / 64.0).to_le_bytes());
    }
    assert_eq!(fnv1a(&dlog_bytes), 0x27b0_f3cc);

    for (k, expected) in [
        (1, 0x8c6a_9878),
        (2, 0x2417_b297),
        (17, 0x2ba4_1e3c),
        (179, 0xe8b6_340a),
        (716, 0x28d3_1438),
        (5000, 0x357a_4c9a),
        (22000, 0xfc51_2a92),
    ] {
        let bytes: Vec<u8> = soliton_cdf(k)
            .into_iter()
            .flat_map(f64::to_le_bytes)
            .collect();
        assert_eq!(fnv1a(&bytes), expected);
    }

    let expected = [
        (1, vec![vec![0], vec![0], vec![0], vec![0], vec![0]]),
        (2, vec![vec![1], vec![1], vec![1], vec![0], vec![1]]),
        (
            17,
            vec![
                vec![3, 14],
                vec![12, 0],
                vec![6, 8],
                vec![15, 16, 13],
                vec![11, 2, 16],
            ],
        ),
        (
            179,
            vec![
                vec![27, 39],
                vec![30, 55],
                vec![155, 125],
                vec![28, 132, 88],
                vec![39, 75, 24],
            ],
        ),
        (
            716,
            vec![
                vec![27, 397],
                vec![567, 592],
                vec![155, 304],
                vec![386, 311, 625],
                vec![39, 433, 382],
            ],
        ),
    ];
    for (k, subsets) in expected {
        let cdf = soliton_cdf(k);
        for (seq, subset) in [0, 1, 2, 41, 1000].into_iter().zip(subsets) {
            assert_eq!(frame_indices(k, &cdf, 4242, seq), subset);
        }
    }
}

#[test]
fn decoder_handles_clean_duplicate_redundant_and_reordered_frames() {
    let payload = test_payload(50_000);
    let encoder = LtEncoder::new(&payload, 1445, 77).unwrap();
    let mut decoder = LtDecoder::new(encoder.source_blocks(), 1445, 77, payload.len()).unwrap();
    assert_eq!(
        decoder.add_frame(0, &encoder.encode(0)).unwrap(),
        AddFrame::Accepted
    );
    assert_eq!(
        decoder.add_frame(0, &encoder.encode(0)).unwrap(),
        AddFrame::Duplicate
    );
    assert_eq!(
        decoder
            .add_frame(
                cycle_length(encoder.source_blocks()),
                &encoder.encode(cycle_length(encoder.source_blocks()))
            )
            .unwrap(),
        AddFrame::Redundant
    );

    let mut reordered = LtDecoder::new(encoder.source_blocks(), 1445, 77, payload.len()).unwrap();
    let mut captured: Vec<_> = (0..encoder.source_blocks() * 3)
        .map(|seq| (u32::from(seq), encoder.encode(u32::from(seq))))
        .collect();
    captured.reverse();
    for (seq, block) in captured {
        if reordered.add_frame(seq, &block).unwrap() == AddFrame::Complete {
            break;
        }
    }
    assert!(reordered.is_complete());
    assert_eq!(reordered.assemble().unwrap(), payload);
}

#[test]
fn lossy_round_trips_stay_within_recorded_bounds() {
    let (payload, frames, emitted_sequences_per_block, overhead) =
        round_trip(512 * 1024, 2933, 23, 0.30);
    assert_eq!(payload, test_payload(512 * 1024));
    // Upstream calls this quantity "wall clock", but it is sequences emitted
    // divided by source blocks -- how much of the carousel the sender had to
    // play, not elapsed time. Timing a decode here would measure the machine.
    assert!(
        emitted_sequences_per_block < 2.8,
        "sender emitted {emitted_sequences_per_block:.2} sequences per source block"
    );
    assert!(overhead < 1.8, "unique frame overhead was {overhead:.2}");
    assert!(frames > 0);
}

#[test]
fn framed_transfer_recovers_after_deterministic_fifteen_percent_loss() {
    let payload = test_payload(300 * 1024);
    let encoder = LtEncoder::new(&payload, 1445, 91).unwrap();
    let mut decoder = LtDecoder::new(encoder.source_blocks(), 1445, 91, payload.len()).unwrap();
    let mut random = SplitMix32::new(91);
    let mut seq = 0_u32;
    while !decoder.is_complete() && seq < u32::from(encoder.source_blocks()) * 10 {
        let block = encoder.encode(seq);
        let frame = pack_frame(
            FrameHeader {
                session_id: 91,
                seq,
                source_blocks: encoder.source_blocks(),
                block_len: 1445,
                total_len: payload.len() as u32,
                payload_fnv: fnv1a(&payload),
                flags: 0,
            },
            &block,
        )
        .unwrap();
        if f64::from(random.next_u32()) * (1.0 / 4_294_967_296.0) >= 0.15 {
            let parsed = parse_frame(&frame).unwrap().unwrap();
            decoder.add_frame(parsed.header.seq, parsed.block).unwrap();
        }
        seq += 1;
    }
    assert!(decoder.is_complete());
    assert_eq!(decoder.assemble().unwrap(), payload);
    assert!(decoder.unique_frames() as f64 / f64::from(encoder.source_blocks()) < 1.3);
}

fn round_trip(
    size: usize,
    block_len: usize,
    session_id: u16,
    loss: f64,
) -> (Vec<u8>, usize, f64, f64) {
    let expected = test_payload(size);
    let encoder = LtEncoder::new(&expected, block_len, session_id).unwrap();
    let mut decoder = LtDecoder::new(
        encoder.source_blocks(),
        block_len,
        session_id,
        expected.len(),
    )
    .unwrap();
    let mut random = SplitMix32::new(u32::from(session_id));
    let mut seq = 0_u32;
    while !decoder.is_complete() && seq < u32::from(encoder.source_blocks()) * 80 + 5000 {
        if f64::from(random.next_u32()) * (1.0 / 4_294_967_296.0) >= loss {
            decoder.add_frame(seq, &encoder.encode(seq)).unwrap();
        }
        seq += 1;
    }
    let unique = decoder.unique_frames();
    let source_blocks = encoder.source_blocks();
    (
        decoder.assemble().unwrap(),
        unique,
        f64::from(seq) / f64::from(source_blocks),
        unique as f64 / f64::from(source_blocks),
    )
}

fn test_payload(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| ((index.wrapping_mul(37) + (index >> 8).wrapping_mul(11)) & 0xff) as u8)
        .collect()
}
