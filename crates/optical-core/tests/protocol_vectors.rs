use optical_core::protocol::{
    FrameHeader, FrameVerdict, HEADER_LEN, SplitMix32, classify_frame, fnv1a, pack_frame,
    parse_frame, stream_identity,
};

fn header(flags: u8) -> FrameHeader {
    FrameHeader {
        session_id: 0xbeef,
        seq: 0x0102_0304,
        source_blocks: 0x0111,
        block_len: 6,
        total_len: 0x00fe_dcba,
        payload_fnv: 0x89ab_cdef,
        flags,
    }
}

fn frame(flags: u8) -> Vec<u8> {
    pack_frame(header(flags), &[1, 2, 3, 4, 5, 6]).unwrap()
}

#[test]
fn canonical_wire_v3_frame_is_exact() {
    let actual = frame(0);
    assert_eq!(HEADER_LEN, 22);
    assert_eq!(
        actual,
        hex_bytes(
            "d1 c3 03 00 ef be 04 03 02 01 11 01 06 00 ba dc fe 00 ef cd ab 89 01 02 03 04 05 06"
        )
    );
    let parsed = parse_frame(&actual).unwrap().unwrap();
    assert_eq!(parsed.header, header(0));
    assert_eq!(parsed.block, [1, 2, 3, 4, 5, 6]);
}

#[test]
fn classification_distinguishes_versions_flags_and_malformed_frames() {
    assert_eq!(classify_frame(&[0xd2, 0xc3, 3, 0]), FrameVerdict::Foreign);
    assert_eq!(
        classify_frame(&[0xd1, 0x0c, 3, 0]),
        FrameVerdict::OlderSender(1)
    );
    assert_eq!(
        classify_frame(&[0xd1, 0x0d, 3, 0]),
        FrameVerdict::OlderSender(2)
    );
    assert_eq!(
        classify_frame(&[0xd1, 0xc3, 2, 0]),
        FrameVerdict::OlderSender(2)
    );
    assert_eq!(
        classify_frame(&[0xd1, 0xc3, 4, 0]),
        FrameVerdict::NewerSender(4)
    );
    assert_eq!(classify_frame(&[0xd1, 0xc3, 0, 0]), FrameVerdict::Malformed);
    assert_eq!(
        classify_frame(&[0xd1, 0xc3, 3, 1]),
        FrameVerdict::UnsupportedFlags(1)
    );

    for len in 0..=HEADER_LEN {
        assert_ne!(classify_frame(&frame(0)[..len]), FrameVerdict::Valid);
    }

    let mut zero_k = frame(0);
    zero_k[10..12].copy_from_slice(&0_u16.to_le_bytes());
    assert_eq!(classify_frame(&zero_k), FrameVerdict::Malformed);
    let mut zero_block_len = frame(0);
    zero_block_len[12..14].copy_from_slice(&0_u16.to_le_bytes());
    assert_eq!(classify_frame(&zero_block_len), FrameVerdict::Malformed);
    let mut zero_total_len = frame(0);
    zero_total_len[14..18].copy_from_slice(&0_u32.to_le_bytes());
    assert_eq!(classify_frame(&zero_total_len), FrameVerdict::Malformed);
    assert_eq!(
        classify_frame(&frame(0)[..HEADER_LEN + 5]),
        FrameVerdict::Malformed
    );
}

#[test]
fn ignorable_flags_parse_and_do_not_change_stream_identity() {
    let baseline = stream_identity(header(0));
    for flag in [0x10, 0x20, 0x40, 0x80] {
        let bytes = frame(flag);
        assert_eq!(classify_frame(&bytes), FrameVerdict::Valid);
        assert_eq!(parse_frame(&bytes).unwrap().unwrap().header.flags, flag);
        assert_eq!(stream_identity(header(flag)), baseline);
    }
    assert_ne!(stream_identity(header(1)), baseline);
}

#[test]
fn fnv_and_splitmix32_match_upstream_integer_vectors() {
    assert_eq!(fnv1a(b""), 0x811c_9dc5);
    assert_eq!(fnv1a(b"hello"), 0x4f9f_2cab);
    assert_eq!(fnv1a(&[0, 1, 2, 127, 128, 254, 255]), 0x07de_e1b4);

    let mut rng = SplitMix32::new(0);
    assert_eq!(
        (0..5).map(|_| rng.next_u32()).collect::<Vec<_>>(),
        vec![
            0x6462_5032,
            0xd9c0_799c,
            0xaf36_2e10,
            0x7fa8_8912,
            0xc467_1b39
        ]
    );
}

fn hex_bytes(value: &str) -> Vec<u8> {
    value
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}
