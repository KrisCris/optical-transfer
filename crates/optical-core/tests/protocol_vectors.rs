use optical_core::protocol::{
    FrameHeader, FrameHeaderV4, FrameVerdict, HEADER_LEN, MAX_SYMBOL_ID, SplitMix32, V4_HEADER_LEN,
    Wire, classify_frame, fnv1a, pack_frame, pack_frame_v4, parse_frame, parse_frame_v4,
    stream_identity, stream_identity_v4,
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
    // Version 4 is this build's own, so a stub is merely malformed; anything
    // past it is a sender newer than this receiver.
    assert_eq!(classify_frame(&[0xd1, 0xc3, 4, 0]), FrameVerdict::Malformed);
    assert_eq!(
        classify_frame(&[0xd1, 0xc3, 5, 0]),
        FrameVerdict::NewerSender(5)
    );
    assert_eq!(classify_frame(&[0xd1, 0xc3, 0, 0]), FrameVerdict::Malformed);
    assert_eq!(
        classify_frame(&[0xd1, 0xc3, 3, 1]),
        FrameVerdict::UnsupportedFlags(1)
    );

    for len in 0..=HEADER_LEN {
        assert_ne!(
            classify_frame(&frame(0)[..len]),
            FrameVerdict::Valid(Wire::V3)
        );
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
        assert_eq!(classify_frame(&bytes), FrameVerdict::Valid(Wire::V3));
        assert_eq!(parse_frame(&bytes).unwrap().unwrap().header.flags, flag);
        assert_eq!(stream_identity(header(flag)), baseline);
    }
    assert_ne!(stream_identity(header(1)), baseline);
}

#[test]
fn canonical_wire_v4_frame_is_exact() {
    let header = FrameHeaderV4 {
        flags: 0,
        checksum: 0x89ab_cdef,
        total_len: 0x00fe_dcba,
        block: 0x02,
        symbol: 0x0001_0203,
    };
    let actual = pack_frame_v4(header, &[1, 2, 3, 4, 5, 6]).unwrap();
    assert_eq!(V4_HEADER_LEN, 16);
    assert_eq!(
        actual,
        hex_bytes("d1 c3 04 00 ef cd ab 89 ba dc fe 00 03 02 01 02 01 02 03 04 05 06")
    );
    assert_eq!(classify_frame(&actual), FrameVerdict::Valid(Wire::V4));
    let parsed = parse_frame_v4(&actual).unwrap();
    assert_eq!(parsed.header, header);
    assert_eq!(parsed.symbol, [1, 2, 3, 4, 5, 6]);
    // Neither parser reads the other's frames.
    assert!(parse_frame(&actual).unwrap().is_none());
    assert!(parse_frame_v4(&frame(0)).is_none());
}

#[test]
fn v4_classification_refuses_what_a_decoder_could_not_use() {
    let header = FrameHeaderV4 {
        flags: 0,
        checksum: 7,
        total_len: 100,
        block: 0,
        symbol: MAX_SYMBOL_ID,
    };
    let good = pack_frame_v4(header, &[9; 10]).unwrap();
    assert_eq!(classify_frame(&good), FrameVerdict::Valid(Wire::V4));
    assert_eq!(parse_frame_v4(&good).unwrap().header.symbol, MAX_SYMBOL_ID);

    // A header with no symbol behind it, or a length of zero.
    for len in 0..=V4_HEADER_LEN {
        assert_ne!(classify_frame(&good[..len]), FrameVerdict::Valid(Wire::V4));
    }
    let mut zero_total = good.clone();
    zero_total[8..12].copy_from_slice(&0_u32.to_le_bytes());
    assert_eq!(classify_frame(&zero_total), FrameVerdict::Malformed);
    // Critical flags are refused; ignorable ones parse and keep the identity.
    let mut critical = good.clone();
    critical[3] = 0x02;
    assert_eq!(classify_frame(&critical), FrameVerdict::UnsupportedFlags(2));
    let mut ignorable = good.clone();
    ignorable[3] = 0x40;
    let parsed = parse_frame_v4(&ignorable).unwrap();
    assert_eq!(
        stream_identity_v4(parsed.header, parsed.symbol.len()),
        stream_identity_v4(header, 10)
    );
    // The identity pins the symbol size, which the frame length carries.
    assert_ne!(
        stream_identity_v4(header, 11),
        stream_identity_v4(header, 10)
    );

    // A packer that would write something a receiver refuses, refuses first.
    assert!(pack_frame_v4(header, &[]).is_err());
    assert!(
        pack_frame_v4(
            FrameHeaderV4 {
                total_len: 0,
                ..header
            },
            &[1]
        )
        .is_err()
    );
    assert!(
        pack_frame_v4(
            FrameHeaderV4 {
                symbol: MAX_SYMBOL_ID + 1,
                ..header
            },
            &[1]
        )
        .is_err()
    );
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
