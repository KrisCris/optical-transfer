//! The receive-side state machine.
//!
//! Deliberately a plain Rust type with no `wasm_bindgen` in sight. The browser
//! and the desktop app are both callers of this logic, not its owners: keeping
//! the state machine ordinary means it can be driven from a test binary at full
//! speed, and means there is exactly one implementation of the protocol rather
//! than one per receiver that drift.
//!
//! Everything reaching [`ReceiverState::add_frame`] came off a camera and is
//! untrusted. Nothing about a payload is exposed until two independent checks
//! agree: the frame header's FNV-1a over the assembled container, and the
//! container's own SHA-256 over the file it carries.
//!
//! Both wire versions arrive here and need nothing from the caller to tell
//! apart: the version byte picks the decoder, wire v3's LT or wire v4's
//! RaptorQ, and everything after decoding is shared.

use crate::{
    OpticalError,
    container::{unpack_file, verify_file},
    fountain::{AddFrame, LtDecoder},
    protocol::{
        FrameHeader, FrameVerdict, StreamIdentity, StreamIdentityV4, Wire, classify_frame, fnv1a,
        parse_frame, parse_frame_v4, stream_identity, stream_identity_v4,
    },
    raptor::RaptorDecoder,
    snippet::{is_snippet, snippet_text},
};

/// Unique frames a stream may spend per source block before the receiver stops
/// holding state for it.
///
/// The upstream measurements put the worst unique-frame overhead at 1.8x at 30%
/// loss and 3.06x emitted-per-block at 50%, so eight is far outside anything a
/// real camera produces. It is not a performance tuning knob: it is the only
/// thing standing between a decoder and unbounded growth, because every frame
/// that fails to peel becomes a `block_len`-sized equation the decoder must
/// keep in case a later frame resolves it.
pub const MAX_FRAMES_PER_SOURCE_BLOCK: usize = 8;

/// Floor under the per-stream frame budget.
///
/// A two-block transfer has no meaningful multiple to take, and a receiver that
/// abandoned it after sixteen frames would be giving up on a transfer a user
/// can see is still running.
pub const MIN_FRAME_BUDGET: usize = 256;

/// Unique frames the receiver will accept into one stream before abandoning it.
pub fn frame_budget(source_blocks: usize) -> usize {
    source_blocks
        .saturating_mul(MAX_FRAMES_PER_SOURCE_BLOCK)
        .max(MIN_FRAME_BUDGET)
}

/// Longest media type the receiver will repeat back to its caller. RFC 6838
/// caps a type or subtype at 127 characters; anything past this is not a media
/// type a sender meant to send.
const MAX_MEDIA_TYPE_LEN: usize = 255;

/// Decoder counters and container dimensions for the current stream.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ReceiverStats {
    /// The stream's wire version: 3 (upstream's LT) or 4 (RaptorQ).
    pub wire: u8,
    pub unique_frames: usize,
    pub duplicate_frames: usize,
    pub redundant_frames: usize,
    /// Source blocks recovered. On v4, symbols collected towards completion:
    /// any that many decode a RaptorQ block, so it is progress all the same.
    pub solved_blocks: usize,
    /// Source blocks on v3, source symbols on v4: one pixel each on the map.
    pub total_blocks: usize,
    pub block_len: usize,
    /// Packed container length, before decompression and file extraction.
    pub total_bytes: usize,
    /// Sequence numbers between the lowest and highest frame seen, inclusive:
    /// roughly what the sender emitted while this receiver watched. Unique
    /// frames over this span is the fraction of the stream actually caught.
    pub seq_span: u32,
    /// Repair equations parked until peeling or elimination can use them. On
    /// v4, repair symbols held for blocks that have not decoded yet.
    pub pending_equations: usize,
    /// Gaussian-elimination passes the decoder has attempted. On v4, RaptorQ
    /// decode attempts.
    pub eliminations: usize,
}

/// What one decoded QR payload did to the transfer.
///
/// `kind` is the tag on the wire to JavaScript, so the variants are the whole
/// receiver API — a browser never inspects protocol state directly.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "kind", rename_all = "camelCase"))]
pub enum ReceiverEvent {
    /// Not ours, or too damaged to say anything true about. Silent on purpose:
    /// the camera decodes every QR code in view, including the ones on a shop
    /// window behind the sender, and narrating those is noise.
    Ignored,
    /// Ours, but this build cannot decode it. The one case where saying nothing
    /// would be worse than saying something — a version mismatch otherwise
    /// looks exactly like bad lighting.
    Incompatible { message: String },
    #[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
    Progress {
        /// This frame started a new stream, including an identity replacement.
        new_stream: bool,
        #[cfg_attr(feature = "serde", serde(flatten))]
        stats: ReceiverStats,
    },
    /// Verified. Both checks passed; these bytes are what the sender sent.
    #[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
    Complete {
        /// A one-frame transfer started and completed in this event.
        new_stream: bool,
        stats: ReceiverStats,
        name: String,
        media_type: String,
        bytes: Vec<u8>,
        is_text: bool,
    },
    /// The stream finished, or ran out of budget, without producing bytes that
    /// verify. State for it has already been discarded.
    Failed { message: String },
}

/// The stream a receiver is locked onto, with the decoder its wire needs.
enum Stream {
    V3 {
        identity: StreamIdentity,
        header: FrameHeader,
        decoder: LtDecoder,
    },
    V4 {
        identity: StreamIdentityV4,
        decoder: RaptorDecoder,
    },
}

impl Stream {
    fn unique_frames(&self) -> usize {
        match self {
            Self::V3 { decoder, .. } => decoder.unique_frames(),
            Self::V4 { decoder, .. } => decoder.unique_frames(),
        }
    }

    fn is_complete(&self) -> bool {
        match self {
            Self::V3 { decoder, .. } => decoder.is_complete(),
            Self::V4 { decoder, .. } => decoder.is_complete(),
        }
    }
}

/// One decoded QR payload, parsed into whichever wire it belongs to.
enum Frame<'a> {
    V3(crate::protocol::ParsedFrame<'a>),
    V4(crate::protocol::ParsedFrameV4<'a>),
}

pub struct ReceiverState {
    stream: Option<Stream>,
    budget: usize,
    duplicate_frames: usize,
    seq_range: Option<(u64, u64)>,
}

impl Default for ReceiverState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiverState {
    pub fn new() -> Self {
        Self {
            stream: None,
            budget: 0,
            duplicate_frames: 0,
            seq_range: None,
        }
    }

    /// Forget the current stream. The next frame starts a fresh transfer.
    pub fn reset(&mut self) {
        self.stream = None;
        self.budget = 0;
        self.duplicate_frames = 0;
        self.seq_range = None;
    }

    /// One byte per source block of the current stream, 1 where the block is
    /// recovered. On v4, one per source symbol, 1 where it arrived as itself or
    /// its block has decoded. Empty when no stream is locked.
    pub fn solved_map(&self) -> Vec<u8> {
        match &self.stream {
            None => Vec::new(),
            Some(Stream::V3 { decoder, .. }) => (0..decoder.source_blocks())
                .map(|index| u8::from(decoder.is_solved(usize::from(index))))
                .collect(),
            Some(Stream::V4 { decoder, .. }) => decoder.solved_map(),
        }
    }

    /// Feed one decoded QR payload in.
    ///
    /// Order matters and is the point of the type: classify before parsing, so
    /// a stray code is never read as a header; validate dimensions before
    /// allocating, so a lying header cannot claim memory; check the frame FNV
    /// before unpacking, so a corrupt container is never inflated; check the
    /// container SHA-256 before returning, so nothing unverified is ever handed
    /// to a caller.
    pub fn add_frame(&mut self, bytes: &[u8]) -> ReceiverEvent {
        let frame = match classify_frame(bytes) {
            FrameVerdict::Valid(Wire::V3) => match parse_frame(bytes) {
                Ok(Some(parsed)) => Frame::V3(parsed),
                _ => return ReceiverEvent::Ignored,
            },
            FrameVerdict::Valid(Wire::V4) => match parse_frame_v4(bytes) {
                Some(parsed) => Frame::V4(parsed),
                None => return ReceiverEvent::Ignored,
            },
            // Silent verdicts. `Foreign` is some other QR code; `Malformed` is
            // ours but unreadable, which a partial camera read produces often
            // enough that complaining about it would be constant.
            FrameVerdict::Foreign | FrameVerdict::Malformed => return ReceiverEvent::Ignored,
            FrameVerdict::OlderSender(version) => {
                return ReceiverEvent::Incompatible {
                    message: format!(
                        "That screen is sending an older Decimen format (v{version}). \
                         Update the sending device."
                    ),
                };
            }
            FrameVerdict::NewerSender(version) => {
                return ReceiverEvent::Incompatible {
                    message: format!(
                        "That screen is sending a newer Decimen format (v{version}). \
                         Update this app to receive it."
                    ),
                };
            }
            FrameVerdict::UnsupportedFlags(_) => {
                return ReceiverEvent::Incompatible {
                    message: "That stream uses a Decimen feature this version cannot read. \
                              Update this app to receive it."
                        .to_owned(),
                };
            }
        };

        // Every header field that has to hold constant, not just the session
        // id: ids are 16 random bits redrawn on each sender restart, so a
        // collision is rare but real, and a mismatched frame fed into the old
        // decoder corrupts it silently — surfacing only as a checksum failure
        // after the whole transfer has run. On v4 the identity is the
        // container checksum, length and symbol size, for the same reason.
        let new_stream = match (&frame, &self.stream) {
            (Frame::V3(parsed), Some(Stream::V3 { identity, .. })) => {
                *identity != stream_identity(parsed.header)
            }
            (Frame::V4(parsed), Some(Stream::V4 { identity, .. })) => {
                *identity != stream_identity_v4(parsed.header, parsed.symbol.len())
            }
            _ => true,
        };
        if new_stream {
            match Self::start(&frame) {
                Ok((stream, blocks)) => {
                    self.budget = frame_budget(blocks);
                    self.duplicate_frames = 0;
                    self.seq_range = None;
                    self.stream = Some(stream);
                }
                Err(message) => {
                    self.reset();
                    return ReceiverEvent::Incompatible { message };
                }
            }
        }

        let stream = self
            .stream
            .as_mut()
            .expect("a locked stream always has a decoder");
        let (added, seq) = match (&frame, &mut *stream) {
            (Frame::V3(parsed), Stream::V3 { decoder, .. }) => (
                decoder.add_frame(parsed.header.seq, parsed.block),
                u64::from(parsed.header.seq),
            ),
            (Frame::V4(parsed), Stream::V4 { decoder, .. }) => (
                decoder.add_symbol(parsed.header.block, parsed.header.symbol, parsed.symbol),
                decoder
                    .layout()
                    .sequence(parsed.header.block, parsed.header.symbol),
            ),
            _ => unreachable!("a frame starts a stream of its own wire"),
        };
        match added {
            Ok(AddFrame::Duplicate) => self.duplicate_frames += 1,
            Ok(_) => {}
            Err(_) => {
                // Unreachable while stream identity pins the block or symbol
                // length, but a decoder that cannot take the frame must not be
                // asked about it either. A v4 block number past the layout
                // lands here too.
                return ReceiverEvent::Ignored;
            }
        }

        let (low, high) = self
            .seq_range
            .map_or((seq, seq), |(low, high)| (low.min(seq), high.max(seq)));
        self.seq_range = Some((low, high));
        let seq_span = u32::try_from(high - low + 1).unwrap_or(u32::MAX);
        let stats = match &*stream {
            Stream::V3 {
                header, decoder, ..
            } => ReceiverStats {
                wire: Wire::V3.version(),
                unique_frames: decoder.unique_frames(),
                duplicate_frames: self.duplicate_frames,
                redundant_frames: decoder.redundant_frames(),
                solved_blocks: decoder.solved_count(),
                total_blocks: usize::from(header.source_blocks),
                block_len: usize::from(header.block_len),
                total_bytes: header.total_len as usize,
                seq_span,
                pending_equations: decoder.pending_equations(),
                eliminations: decoder.eliminations(),
            },
            Stream::V4 { decoder, .. } => {
                let layout = decoder.layout();
                ReceiverStats {
                    wire: Wire::V4.version(),
                    unique_frames: decoder.unique_frames(),
                    duplicate_frames: self.duplicate_frames,
                    redundant_frames: decoder.redundant_frames(),
                    solved_blocks: decoder.solved_count(),
                    total_blocks: layout.symbols() as usize,
                    block_len: layout.symbol_len(),
                    total_bytes: layout.total_len(),
                    seq_span,
                    pending_equations: decoder.pending_symbols(),
                    eliminations: decoder.attempts(),
                }
            }
        };

        if stream.is_complete() {
            let assembled = match &mut *stream {
                Stream::V3 {
                    header, decoder, ..
                } => decoder
                    .assemble()
                    .map(|container| (container, header.payload_fnv)),
                Stream::V4 { identity, decoder } => decoder
                    .take_payload()
                    .map(|container| (container, identity.checksum)),
            };
            self.reset();
            return match assembled {
                Ok((container, checksum)) => finish(&container, checksum, new_stream, stats),
                Err(_) => ReceiverEvent::Failed {
                    message: "The transfer reported itself complete but could not be \
                              assembled."
                        .to_owned(),
                },
            };
        }

        let exhausted = matches!(&*stream, Stream::V4 { decoder, .. } if decoder.is_exhausted());
        if stream.unique_frames() >= self.budget || exhausted {
            let seen = stream.unique_frames();
            self.reset();
            return ReceiverEvent::Failed {
                message: format!(
                    "Gave up after {seen} frames without recovering the file. \
                     Restart the sender and try again."
                ),
            };
        }

        ReceiverEvent::Progress { new_stream, stats }
    }

    /// Build a decoder for a frame's stream, or say why its dimensions are
    /// refused. Also returns the source block count the frame budget scales
    /// with.
    fn start(frame: &Frame<'_>) -> Result<(Stream, usize), String> {
        match frame {
            Frame::V3(parsed) => {
                let header = parsed.header;
                LtDecoder::new(
                    header.source_blocks,
                    usize::from(header.block_len),
                    header.session_id,
                    header.total_len as usize,
                )
                .map(|decoder| {
                    (
                        Stream::V3 {
                            identity: stream_identity(header),
                            header,
                            decoder,
                        },
                        usize::from(header.source_blocks),
                    )
                })
                .map_err(|error| match error {
                    OpticalError::UnsupportedDimensions => format!(
                        "That stream describes a {} byte transfer in {} blocks of {} bytes, \
                         which this build will not decode.",
                        header.total_len, header.source_blocks, header.block_len
                    ),
                    other => other.to_string(),
                })
            }
            Frame::V4(parsed) => {
                let header = parsed.header;
                RaptorDecoder::new(header.total_len as usize, parsed.symbol.len())
                    .map(|decoder| {
                        let symbols = decoder.source_symbols() as usize;
                        (
                            Stream::V4 {
                                identity: stream_identity_v4(header, parsed.symbol.len()),
                                decoder,
                            },
                            symbols,
                        )
                    })
                    .map_err(|error| match error {
                        OpticalError::UnsupportedDimensions => format!(
                            "That stream describes a {} byte transfer in {} byte symbols, \
                             which this build will not decode.",
                            header.total_len,
                            parsed.symbol.len()
                        ),
                        other => other.to_string(),
                    })
            }
        }
    }
}

/// Turn an assembled container into a verified result, or a failure.
///
/// Both checks are independent and both must pass. The frame FNV catches a
/// stream assembled from frames that did not belong together; the container
/// SHA-256 catches a container whose own bytes are wrong. Neither is skipped
/// because the other passed.
fn finish(
    container: &[u8],
    checksum: u32,
    new_stream: bool,
    stats: ReceiverStats,
) -> ReceiverEvent {
    if fnv1a(container) != checksum {
        return ReceiverEvent::Failed {
            message: "The recovered stream did not match its checksum. Nothing was saved."
                .to_owned(),
        };
    }
    let file = match unpack_file(container) {
        Ok(file) => file,
        Err(error) => {
            return ReceiverEvent::Failed {
                message: format!("The recovered file could not be read: {error}."),
            };
        }
    };
    if !verify_file(&file) {
        return ReceiverEvent::Failed {
            message: "The recovered file failed its SHA-256 check. Nothing was saved.".to_owned(),
        };
    }

    if is_snippet(&file) {
        // Upstream decodes a snippet with a fatal UTF-8 decoder, so text that
        // is not text is a failure rather than a screenful of U+FFFD.
        return match snippet_text(&file) {
            Ok(text) => ReceiverEvent::Complete {
                new_stream,
                stats,
                name: file.name.clone(),
                media_type: safe_media_type(&file.media_type),
                bytes: text.as_bytes().to_vec(),
                is_text: true,
            },
            Err(_) => ReceiverEvent::Failed {
                message: "The recovered text was not valid UTF-8. Nothing was saved.".to_owned(),
            },
        };
    }

    ReceiverEvent::Complete {
        new_stream,
        stats,
        name: file.name.clone(),
        media_type: safe_media_type(&file.media_type),
        bytes: file.bytes,
        is_text: false,
    }
}

/// The media type arrived over the optical channel and goes straight into a
/// `Blob` on the other side of the boundary, so it is reduced to the printable
/// ASCII a media type is actually made of before it leaves this crate.
///
/// The container module keeps the sender's bytes as they were, deliberately:
/// it mirrors upstream and is not the trust boundary. This is.
fn safe_media_type(media_type: &str) -> String {
    let cleaned: String = media_type
        .chars()
        .take(MAX_MEDIA_TYPE_LEN)
        .filter(|character| character.is_ascii_graphic() || *character == ' ')
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "application/octet-stream".to_owned()
    } else {
        cleaned.to_owned()
    }
}
