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
    MAX_FILE_BYTES, OpticalError,
    capacity::{
        block_length, fits_in_one_stream, smallest_sufficient_frame_size, source_block_count,
    },
    container::{Compression, PackedFile, pack_file},
    fountain::{LtEncoder, cycle_length},
    protocol::{FrameHeader, fnv1a, pack_frame},
    snippet::pack_snippet,
};

use crate::qr::{Ecc, FrameRenderer};

/// Bytes per frame the sender offers.
///
/// A fixed list, not a free number: each value is an exact QR byte-mode
/// capacity at some version, so a frame fills its code instead of paying for
/// modules it does not use.
pub const FRAME_BYTES_OPTIONS: [usize; 6] = [500, 1000, 1465, 1850, 2331, 2953];

/// Frames per second the sender offers.
///
/// 55 sits just under the 60 Hz ceiling on purpose: on a 120 Hz display it gets
/// a clean two refresh cycles per frame, and on a 60 Hz screen the deliberate
/// 5 fps slip against the refresh clock means frame boundaries drift through
/// the scanout instead of riding it. 120 is for high-refresh displays and a
/// receiver capturing at 120; on a slower display it runs at the refresh rate.
/// The rate is the sender's pacing only and never reaches the wire.
pub const TX_FPS_OPTIONS: [u32; 8] = [10, 15, 20, 24, 30, 55, 60, 120];

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
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self {
            frame_bytes: DEFAULT_FRAME_BYTES,
            frames_per_second: DEFAULT_TX_FPS,
            ecc: Ecc::default(),
            grid_codes: 1,
        }
    }
}

/// Why a transfer could not start. Every variant names something the user can
/// change; none of them is "an error occurred".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartError {
    Empty,
    FileTooLarge {
        bytes: usize,
    },
    /// The payload needs more source blocks than the frame header's `u16` can
    /// number. Reachable well under the file size limit at small frame sizes.
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
            Self::FileTooLarge { bytes } => write!(
                formatter,
                "That file is {} and the limit is {}.",
                human_bytes(*bytes),
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

/// A running stream: the encoder, the header every frame shares, and a
/// sequence number that only ever goes up.
pub struct Transfer {
    encoder: LtEncoder,
    header: FrameHeader,
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
            .field(
                "session_id",
                &format_args!("{:#06x}", self.header.session_id),
            )
            .field("source_blocks", &self.header.source_blocks)
            .field("block_len", &self.header.block_len)
            .field("total_len", &self.header.total_len)
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
        // chose, and making them wait through a 64 MiB gzip to be told no is a
        // worse answer than the same answer given immediately.
        if bytes.len() > MAX_FILE_BYTES {
            return Err(StartError::FileTooLarge { bytes: bytes.len() });
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
        let block_len = block_length(settings.frame_bytes).ok_or(StartError::InvalidFrameSize {
            frame_bytes: settings.frame_bytes,
        })?;
        let payload = &packed.container;

        // The block ceiling, checked against the container that will actually
        // be sent rather than the file the user picked — compression changes
        // the answer, sometimes by enough to matter.
        if !fits_in_one_stream(payload.len(), settings.frame_bytes) {
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
        // Receivers take the block length from the header.
        let blocks = payload.len().div_ceil(block_len).max(1);
        let block_len = payload.len().div_ceil(blocks).max(1);

        let encoder =
            LtEncoder::new(payload, block_len, session_id).map_err(StartError::Packing)?;
        let header = FrameHeader {
            session_id,
            seq: 0,
            source_blocks: encoder.source_blocks(),
            block_len: block_len as u16,
            total_len: payload.len() as u32,
            payload_fnv: fnv1a(payload),
            // No feature bits: this build sends plain wire v3. Spelled out
            // rather than defaulted, so adding a flag is a decision made here
            // and not an omission.
            flags: 0,
        };

        Ok(Self {
            encoder,
            header,
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
        let block = self.encoder.encode(seq);
        pack_frame(FrameHeader { seq, ..self.header }, &block)
            .expect("dimensions were validated when the transfer started")
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

    pub fn source_blocks(&self) -> u16 {
        self.encoder.source_blocks()
    }

    pub fn block_len(&self) -> usize {
        usize::from(self.header.block_len)
    }

    pub fn session_id(&self) -> u16 {
        self.header.session_id
    }

    pub fn sent_frames(&self) -> u32 {
        self.next_seq
    }

    /// Frames in one carousel: a systematic sweep of every block, then an equal
    /// run of repair frames for whatever the sweep dropped.
    pub fn cycle_frames(&self) -> u32 {
        cycle_length(self.encoder.source_blocks())
    }

    /// How many complete carousels have played. A receiver that catches one
    /// clean sweep is done, so this is the honest unit of "how long has this
    /// been up".
    pub fn cycles_completed(&self) -> u32 {
        self.next_seq / self.cycle_frames().max(1)
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
