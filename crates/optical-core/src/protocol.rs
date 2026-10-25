//! Frame headers for both wire versions this build reads.
//!
//! **Wire v3** is upstream Decimen v0.5.3's format, byte for byte: a 22-byte
//! header in front of one LT-coded block. Every receiver here still reads it,
//! because a phone pointed at an upstream sender has to work.
//!
//! **Wire v4** is this build's own, and what its senders send by default. It
//! carries one RaptorQ (RFC 6330) encoding symbol behind a 16-byte header:
//!
//! ```text
//!  0  d1 c3        magic, shared with v3
//!  2  04           version
//!  3  flags        low nibble critical, as in v3
//!  4  checksum     u32 LE, FNV-1a of the whole container; also names the stream
//!  8  total_len    u32 LE, container bytes
//! 12  symbol id    u24 LE, the RaptorQ encoding symbol id (ESI)
//! 15  block        u8, the RaptorQ source block number (SBN)
//! 16  symbol       the rest of the frame; its length is the symbol size
//! ```
//!
//! Nothing else needs to travel. The symbol size is the frame's own length,
//! and the source block layout is a fixed function of `(total_len, symbol
//! size)` that both ends compute (see [`crate::raptor::Layout`]). There is no
//! session id: the container checksum names the stream, so a sender restarted
//! on the same file and settings continues a receiver's transfer instead of
//! starting a new one. Upstream receivers classify a v4 frame as coming from a
//! newer sender and say so, which is the right message.

use crate::error::OpticalError;

/// Wire v3's header length.
pub const HEADER_LEN: usize = 22;
/// Wire v4's header length.
pub const V4_HEADER_LEN: usize = 16;
/// Upstream's wire version, the one [`pack_frame`] writes.
pub const WIRE_VERSION: u8 = 3;
/// This build's own wire version, the one [`pack_frame_v4`] writes.
pub const WIRE_V4: u8 = 4;
pub const CRITICAL_FLAGS: u8 = 0x0f;

const MAGIC0: u8 = 0xd1;
const MAGIC1: u8 = 0xc3;

/// The largest symbol id a v4 frame can carry: 24 bits, as in RFC 6330.
pub const MAX_SYMBOL_ID: u32 = (1 << 24) - 1;

/// Which format a frame is in, or a sender sends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Wire {
    /// Upstream Decimen v0.5.3: LT fountain, 22-byte header.
    V3,
    /// This build's format: RaptorQ, 16-byte header.
    #[default]
    V4,
}

impl Wire {
    pub const ALL: [Self; 2] = [Self::V4, Self::V3];

    pub fn version(self) -> u8 {
        match self {
            Self::V3 => WIRE_VERSION,
            Self::V4 => WIRE_V4,
        }
    }

    /// Bytes of every frame the header takes.
    pub fn header_len(self) -> usize {
        match self {
            Self::V3 => HEADER_LEN,
            Self::V4 => V4_HEADER_LEN,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::V3 => "v3",
            Self::V4 => "v4",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub session_id: u16,
    pub seq: u32,
    pub source_blocks: u16,
    pub block_len: u16,
    pub total_len: u32,
    pub payload_fnv: u32,
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameVerdict {
    /// A well-formed frame in a format this build decodes.
    Valid(Wire),
    Foreign,
    OlderSender(u8),
    NewerSender(u8),
    UnsupportedFlags(u8),
    Malformed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamIdentity {
    pub session_id: u16,
    pub source_blocks: u16,
    pub block_len: u16,
    pub total_len: u32,
    pub payload_fnv: u32,
    pub critical_flags: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedFrame<'a> {
    pub header: FrameHeader,
    pub block: &'a [u8],
}

pub fn pack_frame(header: FrameHeader, block: &[u8]) -> Result<Vec<u8>, OpticalError> {
    if header.source_blocks == 0 || header.block_len == 0 || header.total_len == 0 {
        return Err(OpticalError::UnsupportedDimensions);
    }
    if block.len() != usize::from(header.block_len) {
        return Err(OpticalError::InvalidFrame);
    }

    let mut bytes = vec![0; HEADER_LEN + block.len()];
    bytes[0] = MAGIC0;
    bytes[1] = MAGIC1;
    bytes[2] = WIRE_VERSION;
    bytes[3] = header.flags;
    bytes[4..6].copy_from_slice(&header.session_id.to_le_bytes());
    bytes[6..10].copy_from_slice(&header.seq.to_le_bytes());
    bytes[10..12].copy_from_slice(&header.source_blocks.to_le_bytes());
    bytes[12..14].copy_from_slice(&header.block_len.to_le_bytes());
    bytes[14..18].copy_from_slice(&header.total_len.to_le_bytes());
    bytes[18..22].copy_from_slice(&header.payload_fnv.to_le_bytes());
    bytes[HEADER_LEN..].copy_from_slice(block);
    Ok(bytes)
}

pub fn classify_frame(bytes: &[u8]) -> FrameVerdict {
    if bytes.len() < 4 || bytes[0] != MAGIC0 {
        return FrameVerdict::Foreign;
    }
    if bytes[1] != MAGIC1 {
        return match bytes[1] {
            0x0c => FrameVerdict::OlderSender(1),
            0x0d => FrameVerdict::OlderSender(2),
            _ => FrameVerdict::Foreign,
        };
    }

    match bytes[2] {
        0 => return FrameVerdict::Malformed,
        WIRE_VERSION => {}
        WIRE_V4 => return classify_v4(bytes),
        version if version > WIRE_V4 => return FrameVerdict::NewerSender(version),
        version => return FrameVerdict::OlderSender(version),
    }

    let unknown_critical = bytes[3] & CRITICAL_FLAGS;
    if unknown_critical != 0 {
        return FrameVerdict::UnsupportedFlags(unknown_critical);
    }
    if bytes.len() <= HEADER_LEN {
        return FrameVerdict::Malformed;
    }

    let source_blocks = u16::from_le_bytes([bytes[10], bytes[11]]);
    let block_len = u16::from_le_bytes([bytes[12], bytes[13]]);
    let total_len = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
    if source_blocks == 0 || block_len == 0 || total_len == 0 {
        return FrameVerdict::Malformed;
    }
    if bytes.len() != HEADER_LEN + usize::from(block_len) {
        return FrameVerdict::Malformed;
    }
    FrameVerdict::Valid(Wire::V3)
}

/// The v4 half of [`classify_frame`], once magic and version have matched.
fn classify_v4(bytes: &[u8]) -> FrameVerdict {
    let unknown_critical = bytes[3] & CRITICAL_FLAGS;
    if unknown_critical != 0 {
        return FrameVerdict::UnsupportedFlags(unknown_critical);
    }
    // At least one symbol byte, and no more than a RaptorQ symbol can hold.
    if bytes.len() <= V4_HEADER_LEN || bytes.len() - V4_HEADER_LEN > usize::from(u16::MAX) {
        return FrameVerdict::Malformed;
    }
    if u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) == 0 {
        return FrameVerdict::Malformed;
    }
    FrameVerdict::Valid(Wire::V4)
}

/// Parse a wire v3 frame. `None` for anything else, v4 frames included.
pub fn parse_frame(bytes: &[u8]) -> Result<Option<ParsedFrame<'_>>, OpticalError> {
    if classify_frame(bytes) != FrameVerdict::Valid(Wire::V3) {
        return Ok(None);
    }
    let header = FrameHeader {
        session_id: u16::from_le_bytes([bytes[4], bytes[5]]),
        seq: u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
        source_blocks: u16::from_le_bytes([bytes[10], bytes[11]]),
        block_len: u16::from_le_bytes([bytes[12], bytes[13]]),
        total_len: u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]),
        payload_fnv: u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]),
        flags: bytes[3],
    };
    Ok(Some(ParsedFrame {
        header,
        block: &bytes[HEADER_LEN..],
    }))
}

pub fn stream_identity(header: FrameHeader) -> StreamIdentity {
    StreamIdentity {
        session_id: header.session_id,
        source_blocks: header.source_blocks,
        block_len: header.block_len,
        total_len: header.total_len,
        payload_fnv: header.payload_fnv,
        critical_flags: header.flags & CRITICAL_FLAGS,
    }
}

/// One wire v4 frame's header. Every field but `block` and `symbol` is the
/// same in every frame of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeaderV4 {
    pub flags: u8,
    /// FNV-1a over the whole container, checked before it is unpacked.
    pub checksum: u32,
    /// Container bytes.
    pub total_len: u32,
    /// RaptorQ source block number.
    pub block: u8,
    /// RaptorQ encoding symbol id within `block`, at most [`MAX_SYMBOL_ID`].
    pub symbol: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedFrameV4<'a> {
    pub header: FrameHeaderV4,
    pub symbol: &'a [u8],
}

/// What a v4 receiver pins for the life of a transfer. The symbol size is the
/// frame length, so a sender restarted at a different bytes-per-frame is a
/// different stream even for the same file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamIdentityV4 {
    pub checksum: u32,
    pub total_len: u32,
    pub symbol_len: u16,
    pub critical_flags: u8,
}

pub fn pack_frame_v4(header: FrameHeaderV4, symbol: &[u8]) -> Result<Vec<u8>, OpticalError> {
    if header.total_len == 0
        || symbol.is_empty()
        || symbol.len() > usize::from(u16::MAX)
        || header.symbol > MAX_SYMBOL_ID
    {
        return Err(OpticalError::UnsupportedDimensions);
    }
    let mut bytes = Vec::with_capacity(V4_HEADER_LEN + symbol.len());
    bytes.extend_from_slice(&[MAGIC0, MAGIC1, WIRE_V4, header.flags]);
    bytes.extend_from_slice(&header.checksum.to_le_bytes());
    bytes.extend_from_slice(&header.total_len.to_le_bytes());
    bytes.extend_from_slice(&(header.symbol | (u32::from(header.block) << 24)).to_le_bytes());
    bytes.extend_from_slice(symbol);
    Ok(bytes)
}

/// Parse a wire v4 frame. `None` for anything else, v3 frames included.
pub fn parse_frame_v4(bytes: &[u8]) -> Option<ParsedFrameV4<'_>> {
    if classify_frame(bytes) != FrameVerdict::Valid(Wire::V4) {
        return None;
    }
    let word =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let id = word(12);
    Some(ParsedFrameV4 {
        header: FrameHeaderV4 {
            flags: bytes[3],
            checksum: word(4),
            total_len: word(8),
            block: (id >> 24) as u8,
            symbol: id & MAX_SYMBOL_ID,
        },
        symbol: &bytes[V4_HEADER_LEN..],
    })
}

pub fn stream_identity_v4(header: FrameHeaderV4, symbol_len: usize) -> StreamIdentityV4 {
    StreamIdentityV4 {
        checksum: header.checksum,
        total_len: header.total_len,
        symbol_len: symbol_len as u16,
        critical_flags: header.flags & CRITICAL_FLAGS,
    }
}

pub fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

pub struct SplitMix32(u32);

impl SplitMix32 {
    pub fn new(seed: u32) -> Self {
        Self(seed)
    }

    pub fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x9e37_79b9);
        let mut value = self.0 ^ (self.0 >> 16);
        value = value.wrapping_mul(0x21f0_aaad);
        value ^= value >> 15;
        value = value.wrapping_mul(0x735a_2d97);
        value ^ (value >> 15)
    }
}
