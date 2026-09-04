use crate::error::OpticalError;

pub const HEADER_LEN: usize = 22;
pub const WIRE_VERSION: u8 = 3;
pub const CRITICAL_FLAGS: u8 = 0x0f;

const MAGIC0: u8 = 0xd1;
const MAGIC1: u8 = 0xc3;

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
    Valid,
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
        version if version > WIRE_VERSION => return FrameVerdict::NewerSender(version),
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
    FrameVerdict::Valid
}

pub fn parse_frame(bytes: &[u8]) -> Result<Option<ParsedFrame<'_>>, OpticalError> {
    if classify_frame(bytes) != FrameVerdict::Valid {
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
