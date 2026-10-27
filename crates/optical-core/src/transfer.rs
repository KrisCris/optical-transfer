//! Turning a chosen file or a pasted snippet into an endless frame stream.
//!
//! The fountain carousel has no end and needs no handshake: a receiver locking
//! on anywhere rebuilds the payload, so this type's whole job is to hand out
//! the next sequence number forever and let `optical-core` compose the frame.
//!
//! What it does own is every refusal that has to happen *before* a stream
//! starts. A stream that cannot complete looks exactly like a stream that is
//! merely slow, so each one names the setting that fixes it.

use std::fmt;

use crate::{
    MAX_FILE_BYTES, OpticalError, V3_MAX_FILE_BYTES,
    capacity::{
        block_length, fits_in_one_stream, smallest_sufficient_frame_size, source_block_count,
    },
    container::{Compression, PackedFile, pack_file},
    fountain::{LtEncoder, cycle_length},
    protocol::{FrameHeader, FrameHeaderV4, Wire, fnv1a, pack_frame, pack_frame_v4},
    raptor::RaptorEncoder,
    snippet::pack_snippet,
};

use crate::qr::{Ecc, FrameRenderer};

/// Bytes per frame the sender offers.
///
/// A fixed list, not a free number: each value is an exact QR byte-mode
/// capacity at some version, so a frame fills its code instead of paying for
/// modules it does not use.
pub const FRAME_BYTES_OPTIONS: [usize; 6] = [500, 1000, 1465, 1850, 2331, 2953];

/// Frames per second the sender offers, as ceilings.
///
/// Both senders hold each code a whole number of display refreshes, the fewest
/// not much shorter than 1/fps, because a phone camera only reads a code that
/// holds still: 60 fps on a 144 Hz display alternated three refreshes and two,
/// and an iPhone missed most of the two-refresh codes. 36 is about 28 ms a
/// code, enough for a phone filming at 60 fps: four refreshes at 144 Hz, and
/// two at 60 Hz, where it runs at 30. 48 and 72 are three and two refreshes at
/// 144 Hz and 120 one at 120 Hz, for a receiver capturing at 120. The rate is
/// the sender's pacing only and never reaches the wire.
pub const TX_FPS_OPTIONS: [u32; 10] = [10, 15, 20, 24, 30, 36, 48, 60, 72, 120];

/// The pair upstream's own no-signal hint walks a struggling sender down to.
///
/// Chosen over upstream's *defaults* (2953 bytes at 60 fps) because those
/// assume a high-refresh sender and a measured rig; this build has no field
/// data yet, and a first transfer that works slowly beats one that fails fast.
/// The two numbers travel together — 1465 belongs with 24, not with an
/// invented third value.
pub const DEFAULT_FRAME_BYTES: usize = 1465;
pub const DEFAULT_TX_FPS: u32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferSettings {
    pub frame_bytes: usize,
    pub frames_per_second: u32,
    pub ecc: Ecc,
    pub grid_codes: usize,
    /// The wire format. v4 unless someone needs upstream's receiver to read it.
    pub wire: Wire,
    /// Three frames per code, one in each of the red, green and blue channels.
    /// See [`crate::qr::compose`].
    pub color: bool,
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self {
            frame_bytes: DEFAULT_FRAME_BYTES,
            frames_per_second: DEFAULT_TX_FPS,
            ecc: Ecc::default(),
            grid_codes: 1,
            wire: Wire::default(),
            color: false,
        }
    }
}

impl TransferSettings {
    /// Frames each code on screen carries at once.
    pub fn layers(&self) -> usize {
        if self.color { 3 } else { 1 }
    }

    /// Payload bytes on the wire per second at these settings, before loss.
    pub fn wire_rate(&self) -> f64 {
        let payload = self.frame_bytes.saturating_sub(self.wire.header_len());
        (self.frames_per_second as usize * self.grid_codes * self.layers() * payload) as f64
    }
}

/// Why a transfer could not start. Every variant names something the user can
/// change; none of them is "an error occurred".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartError {
    Empty,
    FileTooLarge {
        bytes: usize,
        /// The limit for the chosen wire: upstream's v3 stops at 64 MiB.
        limit: usize,
    },
    /// The packed container is over what wire v3 carries. Upstream's 64 MiB
    /// limits the container, the file is checked before packing, and a file
    /// just under the limit packs into a container just over it. Wire v4
    /// carries any container a permitted file makes.
    ContainerTooLarge {
        bytes: usize,
        limit: usize,
    },
    /// The payload needs more source blocks than a v3 frame header's `u16` can
    /// number. Reachable well under the file size limit at small frame sizes.
    /// Wire v4 numbers symbols in 24 bits per block and has no such ceiling.
    TooManyBlocks {
        blocks: usize,
        frame_bytes: usize,
        /// A frame size from [`FRAME_BYTES_OPTIONS`] that does work, when one
        /// does. `None` means no offered setting carries this payload.
        suggestion: Option<usize>,
    },
    /// The requested bytes per frame do not fit any QR version at this ECC.
    FrameTooDenseForEcc {
        frame_bytes: usize,
        ecc: Ecc,
    },
    InvalidFrameSize {
        frame_bytes: usize,
    },
    Packing(OpticalError),
}

impl fmt::Display for StartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("There is nothing to send yet."),
            Self::FileTooLarge { bytes, limit } => write!(
                formatter,
                "That file is {} and the limit is {}.",
                human_bytes(*bytes),
                human_bytes(*limit)
            ),
            Self::ContainerTooLarge { limit, .. } => write!(
                formatter,
                "With its name and checksum this comes to just over the {} wire v3 carries. \
                 Choose wire v4, which carries up to {}.",
                human_bytes(*limit),
                human_bytes(MAX_FILE_BYTES)
            ),
            Self::TooManyBlocks {
                blocks,
                frame_bytes,
                suggestion,
            } => match suggestion {
                Some(suggestion) => write!(
                    formatter,
                    "At {frame_bytes} bytes per frame this needs {blocks} blocks, and a frame \
                     can only number 65535. Raise bytes per frame to {suggestion}."
                ),
                None => write!(
                    formatter,
                    "At {frame_bytes} bytes per frame this needs {blocks} blocks, and a frame \
                     can only number 65535. No available frame size can carry it."
                ),
            },
            Self::FrameTooDenseForEcc { frame_bytes, ecc } => write!(
                formatter,
                "{frame_bytes} bytes will not fit a QR code at ECC {}. Lower the ECC level or \
                 the bytes per frame.",
                ecc.label()
            ),
            Self::InvalidFrameSize { frame_bytes } => write!(
                formatter,
                "{frame_bytes} bytes per frame leaves no room for a payload."
            ),
            Self::Packing(error) => {
                write!(formatter, "The payload could not be prepared: {error}.")
            }
        }
    }
}

impl std::error::Error for StartError {}

/// The fountain behind a stream, in the wire format it was started with.
enum Encoder {
    V3 {
        encoder: LtEncoder,
        /// The header every frame shares but for `seq`.
        header: FrameHeader,
    },
    V4 {
        encoder: RaptorEncoder,
        checksum: u32,
    },
}

/// A running stream: the encoder, the header every frame shares, and a
/// sequence number that only ever goes up.
pub struct Transfer {
    encoder: Encoder,
    next_seq: u32,
    settings: TransferSettings,
    compression: Compression,
    original_size: usize,
    transmitted_size: usize,
    name: String,
}

/// Names the stream rather than dumping the encoder's block buffer, which is
/// the whole payload and would make a failed assertion unreadable.
impl fmt::Debug for Transfer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Transfer")
            .field("name", &self.name)
            .field("wire", &self.settings.wire)
            .field("stream_id", &format_args!("{:#010x}", self.stream_id()))
            .field("source_blocks", &self.source_blocks())
            .field("block_len", &self.block_len())
            .field("transmitted_size", &self.transmitted_size)
            .field("next_seq", &self.next_seq)
            .finish()
    }
}

impl Transfer {
    pub fn from_file(
        name: &str,
        media_type: &str,
        bytes: &[u8],
        settings: TransferSettings,
        session_id: u16,
    ) -> Result<Self, StartError> {
        if bytes.is_empty() {
            return Err(StartError::Empty);
        }
        // Before compression, deliberately: the limit is on the file the user
        // chose, and making them wait through a 128 MiB gzip to be told no is
        // a worse answer than the same answer given immediately.
        let limit = file_limit(settings.wire);
        if bytes.len() > limit {
            return Err(StartError::FileTooLarge {
                bytes: bytes.len(),
                limit,
            });
        }
        let packed = pack_file(name, media_type, bytes).map_err(StartError::Packing)?;
        Self::from_packed(name.to_owned(), packed, settings, session_id)
    }

    pub fn from_text(
        text: &str,
        settings: TransferSettings,
        session_id: u16,
    ) -> Result<Self, StartError> {
        if text.trim().is_empty() {
            return Err(StartError::Empty);
        }
        let packed = pack_snippet(text).map_err(StartError::Packing)?;
        Self::from_packed("snippet.txt".to_owned(), packed, settings, session_id)
    }

    fn from_packed(
        name: String,
        packed: PackedFile,
        settings: TransferSettings,
        session_id: u16,
    ) -> Result<Self, StartError> {
        let block_len = block_length(settings.frame_bytes, settings.wire).ok_or(
            StartError::InvalidFrameSize {
                frame_bytes: settings.frame_bytes,
            },
        )?;
        let payload = &packed.container;

        if settings.wire == Wire::V3 && payload.len() > V3_MAX_FILE_BYTES {
            return Err(StartError::ContainerTooLarge {
                bytes: payload.len(),
                limit: V3_MAX_FILE_BYTES,
            });
        }

        // The block ceiling, checked against the container that will actually
        // be sent rather than the file the user picked — compression changes
        // the answer, sometimes by enough to matter. Only v3 numbers its
        // blocks in 16 bits.
        if settings.wire == Wire::V3 && !fits_in_one_stream(payload.len(), settings.frame_bytes) {
            return Err(StartError::TooManyBlocks {
                blocks: source_block_count(payload.len(), settings.frame_bytes).unwrap_or(0),
                frame_bytes: settings.frame_bytes,
                suggestion: smallest_sufficient_frame_size(payload.len(), &FRAME_BYTES_OPTIONS),
            });
        }

        // Generate one code now rather than discovering at frame 1 that the
        // density and ECC cannot coexist. A stream that dies on its first frame
        // has already told the user it started.
        FrameRenderer::new(settings.ecc)
            .render(&vec![0_u8; settings.frame_bytes])
            .map_err(|_| StartError::FrameTooDenseForEcc {
                frame_bytes: settings.frame_bytes,
                ecc: settings.ecc,
            })?;

        // The frame size is a ceiling, not a quota. Keep the block count the
        // ceiling implies and spread the payload evenly across it, so a short
        // text rides a small, coarse code instead of a padded version 40.
        // Receivers take the block length from the header, or on v4 from the
        // frame length.
        let blocks = payload.len().div_ceil(block_len).max(1);
        let block_len = payload.len().div_ceil(blocks).max(1);

        let encoder = match settings.wire {
            Wire::V3 => {
                let encoder =
                    LtEncoder::new(payload, block_len, session_id).map_err(StartError::Packing)?;
                let header = FrameHeader {
                    session_id,
                    seq: 0,
                    source_blocks: encoder.source_blocks(),
                    block_len: block_len as u16,
                    total_len: payload.len() as u32,
                    payload_fnv: fnv1a(payload),
                    // No feature bits: plain wire v3. Spelled out rather than
                    // defaulted, so adding a flag is a decision made here and
                    // not an omission.
                    flags: 0,
                };
                Encoder::V3 { encoder, header }
            }
            // No session id on v4: the checksum names the stream, so the same
            // file at the same settings is the same stream to a receiver.
            Wire::V4 => Encoder::V4 {
                encoder: RaptorEncoder::new(payload, block_len).map_err(StartError::Packing)?,
                checksum: fnv1a(payload),
            },
        };

        Ok(Self {
            encoder,
            next_seq: 0,
            settings,
            compression: packed.compression,
            original_size: packed.original_size,
            transmitted_size: packed.transmitted_size,
            name,
        })
    }

    /// The next frame's wire bytes. There is no end to this.
    pub fn next_payload(&mut self) -> Vec<u8> {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        match &self.encoder {
            Encoder::V3 { encoder, header } => {
                pack_frame(FrameHeader { seq, ..*header }, &encoder.encode(seq))
            }
            Encoder::V4 { encoder, checksum } => {
                let (block, symbol, bytes) = encoder.encode(seq);
                pack_frame_v4(
                    FrameHeaderV4 {
                        flags: 0,
                        checksum: *checksum,
                        total_len: encoder.layout().total_len() as u32,
                        block,
                        symbol,
                    },
                    &bytes,
                )
            }
        }
        .expect("dimensions were validated when the transfer started")
    }

    /// The frames one code shows at once: one, or one per colour channel.
    pub fn next_layers(&mut self) -> Vec<Vec<u8>> {
        (0..self.settings.layers())
            .map(|_| self.next_payload())
            .collect()
    }

    pub fn renderer(&self) -> FrameRenderer {
        FrameRenderer::new(self.settings.ecc)
    }

    pub fn settings(&self) -> TransferSettings {
        self.settings
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn wire(&self) -> Wire {
        self.settings.wire
    }

    /// Source blocks on v3; source symbols, across every RaptorQ block, on v4.
    /// Either way, the frames one clean systematic sweep takes.
    pub fn source_blocks(&self) -> u32 {
        match &self.encoder {
            Encoder::V3 { encoder, .. } => u32::from(encoder.source_blocks()),
            Encoder::V4 { encoder, .. } => encoder.layout().symbols(),
        }
    }

    /// Payload bytes per frame.
    pub fn block_len(&self) -> usize {
        match &self.encoder {
            Encoder::V3 { header, .. } => usize::from(header.block_len),
            Encoder::V4 { encoder, .. } => encoder.layout().symbol_len(),
        }
    }

    /// What names the stream on the wire: v3's random session id, or v4's
    /// container checksum.
    pub fn stream_id(&self) -> u32 {
        match &self.encoder {
            Encoder::V3 { header, .. } => u32::from(header.session_id),
            Encoder::V4 { checksum, .. } => *checksum,
        }
    }

    pub fn sent_frames(&self) -> u32 {
        self.next_seq
    }

    /// Frames in one v3 carousel: a systematic sweep of every block, then an
    /// equal run of repair frames for whatever the sweep dropped. `None` on
    /// v4, which sweeps once and then sends fresh repair symbols forever.
    pub fn cycle_frames(&self) -> Option<u32> {
        match &self.encoder {
            Encoder::V3 { encoder, .. } => Some(cycle_length(encoder.source_blocks())),
            Encoder::V4 { .. } => None,
        }
    }

    pub fn compression(&self) -> Compression {
        self.compression
    }

    pub fn original_size(&self) -> usize {
        self.original_size
    }

    pub fn transmitted_size(&self) -> usize {
        self.transmitted_size
    }
}

/// The largest file a sender accepts for this wire.
pub fn file_limit(wire: Wire) -> usize {
    match wire {
        Wire::V3 => V3_MAX_FILE_BYTES,
        Wire::V4 => MAX_FILE_BYTES,
    }
}

/// A size a person can read, in the binary units both senders and the
/// receiver use (1 KiB = 1024 B).
pub fn human_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KIB {
        format!("{bytes:.0} B")
    } else if bytes < KIB * KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{:.2} MiB", bytes / KIB / KIB)
    }
}
