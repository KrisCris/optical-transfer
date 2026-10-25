//! How much payload a stream carries at a given frame size.
//!
//! Wire v4 has no block ceiling (RaptorQ symbol ids are 24 bits per block, up
//! to 255 blocks), so everything below but [`block_length`] is about wire v3.
//!
//! The limit these functions describe is not [`V3_MAX_FILE_BYTES`]. The frame
//! header numbers source blocks in a `u16`, so a large payload at a small
//! bytes-per-frame runs out of block *numbers* long before it runs out of the
//! file size limit: at 500 bytes per frame the real ceiling is about 30 MB, not
//! 64. A sender has to catch that before it starts streaming — a stream that
//! cannot be completed looks exactly like a stream that is merely slow — and it
//! has to name the setting that fixes it.
//!
//! Every function here takes a *frame* size and takes the header's cut itself,
//! because the one mistake worth designing out is a caller that passes a block
//! length where a frame size belongs. That is a 22-byte error, silent, and it
//! only shows up as an undecodable stream.

use crate::{
    V3_MAX_FILE_BYTES,
    protocol::{HEADER_LEN, Wire},
};

/// `k` is a `u16` in the frame header.
pub const MAX_SOURCE_BLOCKS: usize = u16::MAX as usize;

/// Payload bytes per frame, once `wire`'s header has taken its cut.
///
/// `None` when the frame has no room left for a payload at all.
pub fn block_length(frame_bytes: usize, wire: Wire) -> Option<usize> {
    frame_bytes
        .checked_sub(wire.header_len())
        .filter(|len| *len > 0)
}

/// Source blocks a payload splits into at this v3 frame size.
pub fn source_block_count(payload_bytes: usize, frame_bytes: usize) -> Option<usize> {
    Some(payload_bytes.div_ceil(block_length(frame_bytes, Wire::V3)?))
}

/// Can this payload be sent as one stream at this frame size?
pub fn fits_in_one_stream(payload_bytes: usize, frame_bytes: usize) -> bool {
    matches!(source_block_count(payload_bytes, frame_bytes), Some(count) if count <= MAX_SOURCE_BLOCKS)
}

/// The smallest bytes-per-frame that can carry this payload at all.
///
/// `None` only for a payload beyond [`V3_MAX_FILE_BYTES`], which a v3 stream
/// cannot carry in the first place.
pub fn minimum_frame_bytes(payload_bytes: usize) -> Option<usize> {
    if payload_bytes > V3_MAX_FILE_BYTES {
        return None;
    }
    Some(payload_bytes.div_ceil(MAX_SOURCE_BLOCKS) + HEADER_LEN)
}

/// The smallest offered setting that works.
///
/// Separate from [`minimum_frame_bytes`] so a sender can name a value that is
/// actually in its own list of choices rather than the bare arithmetic minimum:
/// telling a user to pick 1045 when the control offers 1000 and 1465 is telling
/// them nothing.
pub fn smallest_sufficient_frame_size(payload_bytes: usize, options: &[usize]) -> Option<usize> {
    let minimum = minimum_frame_bytes(payload_bytes)?;
    options
        .iter()
        .copied()
        .filter(|option| *option >= minimum)
        .min()
}
