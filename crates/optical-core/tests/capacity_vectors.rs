//! Ported from upstream `tests/frame-capacity.test.ts`.
//!
//! The ceiling these functions describe is not the file size limit: `k` is a
//! u16 in the frame header, so a large payload at a small frame size runs out
//! of block *numbers* long before it runs out of bytes. The sender has to catch
//! that before it starts streaming and name the setting that fixes it.

use optical_core::{
    V3_MAX_FILE_BYTES as MAX_FILE_BYTES,
    capacity::{
        MAX_SOURCE_BLOCKS, block_length, fits_in_one_stream, minimum_frame_bytes,
        smallest_sufficient_frame_size, source_block_count,
    },
    protocol::{HEADER_LEN, V4_HEADER_LEN, Wire},
};

/// The sender's real bytes-per-frame choices, so these hold for what is on
/// offer rather than a copy that can drift.
const OFFERED: [usize; 6] = [500, 1000, 1465, 1850, 2331, 2953];

#[test]
fn the_header_takes_its_cut_off_every_frame() {
    assert_eq!(block_length(2953, Wire::V3), Some(2953 - HEADER_LEN));
    assert_eq!(block_length(500, Wire::V3), Some(500 - HEADER_LEN));
    // A frame with no room for a payload is not a frame.
    assert_eq!(block_length(HEADER_LEN, Wire::V3), None);
    assert_eq!(block_length(0, Wire::V3), None);
    // Wire v4's header is six bytes shorter, and every frame gets them back.
    assert_eq!(block_length(2953, Wire::V4), Some(2953 - V4_HEADER_LEN));
    assert_eq!(block_length(V4_HEADER_LEN, Wire::V4), None);
}

#[test]
fn block_count_rounds_up_because_a_partial_block_still_needs_a_frame() {
    // Derived from block_length rather than written out: these boundaries move
    // whenever the header does, and a wire-format change should not look like a
    // capacity bug.
    let per_frame = block_length(2953, Wire::V3).unwrap();
    assert_eq!(source_block_count(1, 2953), Some(1));
    assert_eq!(source_block_count(per_frame, 2953), Some(1));
    assert_eq!(source_block_count(per_frame + 1, 2953), Some(2));
    assert_eq!(source_block_count(10 * per_frame, 2953), Some(10));
}

#[test]
fn the_block_ceiling_bites_well_below_the_file_size_limit() {
    // The whole reason the check exists: at the smallest offered frame size you
    // run out of block numbers around 30 MB, not 64.
    assert!(!fits_in_one_stream(30 * 1024 * 1024, 500));
    assert!(fits_in_one_stream(20 * 1024 * 1024, 500));
    assert!(fits_in_one_stream(MAX_FILE_BYTES, 2953));
}

#[test]
fn minimum_frame_bytes_is_the_smallest_frame_size_that_actually_fits() {
    for payload in [1, 1000, 30 * 1024 * 1024, MAX_FILE_BYTES] {
        let minimum = minimum_frame_bytes(payload).unwrap();
        assert!(
            fits_in_one_stream(payload, minimum),
            "{payload} does not fit at {minimum}"
        );
        // ...and it really is the smallest, unless we are already at the floor
        // where a single block covers everything.
        if source_block_count(payload, minimum) > Some(1) {
            assert!(
                !fits_in_one_stream(payload, minimum - 1),
                "{payload} unexpectedly still fits at {}",
                minimum - 1
            );
        }
    }
}

#[test]
fn the_suggested_option_is_one_the_user_can_actually_pick() {
    // The sender puts this number in front of the user, so it has to be both a
    // value they can choose and one that resolves the error.
    for payload in [1, 1000, 30 * 1024 * 1024, MAX_FILE_BYTES] {
        let suggestion = smallest_sufficient_frame_size(payload, &OFFERED)
            .unwrap_or_else(|| panic!("no offered frame size carries {payload} bytes"));
        assert!(OFFERED.contains(&suggestion));
        assert!(fits_in_one_stream(payload, suggestion));
    }
    // The largest legal payload needs roughly 1045 bytes per frame, so every
    // option above that works and the two smallest do not.
    assert_eq!(
        smallest_sufficient_frame_size(MAX_FILE_BYTES, &OFFERED),
        Some(1465)
    );
    assert_eq!(smallest_sufficient_frame_size(1, &OFFERED), Some(500));
    // An option list with nothing large enough says so rather than guessing.
    assert_eq!(
        smallest_sufficient_frame_size(MAX_FILE_BYTES, &[500, 1000]),
        None
    );
}

#[test]
fn the_ceiling_is_the_wire_type_and_not_a_chosen_number() {
    assert_eq!(MAX_SOURCE_BLOCKS, usize::from(u16::MAX));
    // The minimum rounds the block length up, so the resulting block count sits
    // just under the ceiling rather than exactly on it. What has to hold is
    // that it fits and that nothing smaller does.
    let frame_bytes = minimum_frame_bytes(MAX_FILE_BYTES).unwrap();
    let blocks = source_block_count(MAX_FILE_BYTES, frame_bytes).unwrap();
    assert!(blocks <= MAX_SOURCE_BLOCKS);
    assert!(!fits_in_one_stream(MAX_FILE_BYTES, frame_bytes - 1));
    // A payload that cannot be packed has no answer to give.
    assert_eq!(minimum_frame_bytes(MAX_FILE_BYTES + 1), None);
}
