//! The receive-side state machine.
//!
//! Deliberately a plain Rust type with no `wasm_bindgen` in sight. The browser
//! is one caller of this logic, not its owner: keeping the state machine
//! ordinary means it can be driven from a test binary at full speed, and means
//! there is exactly one implementation of the protocol rather than a Rust one
//! and a JavaScript one that drift.
//!
//! Everything reaching [`ReceiverState::add_frame`] came off a camera and is
//! untrusted. Nothing about a payload is exposed until two independent checks
//! agree: the frame header's FNV-1a over the assembled container, and the
//! container's own SHA-256 over the file it carries.

use optical_core::{
    OpticalError,
    container::{unpack_file, verify_file},
    fountain::LtDecoder,
    protocol::{
        FrameHeader, FrameVerdict, StreamIdentity, classify_frame, fnv1a, parse_frame,
        stream_identity,
    },
    snippet::{is_snippet, snippet_text},
};
use serde::Serialize;

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
pub fn frame_budget(source_blocks: u16) -> usize {
    usize::from(source_blocks)
        .saturating_mul(MAX_FRAMES_PER_SOURCE_BLOCK)
        .max(MIN_FRAME_BUDGET)
}

/// Longest media type the receiver will repeat back to its caller. RFC 6838
/// caps a type or subtype at 127 characters; anything past this is not a media
/// type a sender meant to send.
const MAX_MEDIA_TYPE_LEN: usize = 255;

/// What one decoded QR payload did to the transfer.
///
/// `kind` is the tag on the wire to JavaScript, so the variants are the whole
/// receiver API — a browser never inspects protocol state directly.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReceiverEvent {
    /// Not ours, or too damaged to say anything true about. Silent on purpose:
    /// the camera decodes every QR code in view, including the ones on a shop
    /// window behind the sender, and narrating those is noise.
    Ignored,
    /// Ours, but this build cannot decode it. The one case where saying nothing
    /// would be worse than saying something — a version mismatch otherwise
    /// looks exactly like bad lighting.
    Incompatible { message: String },
    #[serde(rename_all = "camelCase")]
    Progress {
        unique_frames: usize,
        solved_blocks: usize,
        total_blocks: usize,
    },
    /// Verified. Both checks passed; these bytes are what the sender sent.
    #[serde(rename_all = "camelCase")]
    Complete {
        name: String,
        media_type: String,
        bytes: Vec<u8>,
        is_text: bool,
    },
    /// The stream finished, or ran out of budget, without producing bytes that
    /// verify. State for it has already been discarded.
    Failed { message: String },
}

pub struct ReceiverState {
    stream: Option<StreamIdentity>,
    decoder: Option<LtDecoder>,
    budget: usize,
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
            decoder: None,
            budget: 0,
        }
    }

    /// Forget the current stream. The next frame starts a fresh transfer.
    pub fn reset(&mut self) {
        self.stream = None;
        self.decoder = None;
        self.budget = 0;
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
        match classify_frame(bytes) {
            FrameVerdict::Valid => {}
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
        }

        // `classify_frame` already answered `Valid`, so this cannot be `None`.
        let Ok(Some(parsed)) = parse_frame(bytes) else {
            return ReceiverEvent::Ignored;
        };
        let header = parsed.header;

        // Every header field that has to hold constant, not just the session
        // id: ids are 16 random bits redrawn on each sender restart, so a
        // collision is rare but real, and a mismatched frame fed into the old
        // decoder corrupts it silently — surfacing only as a checksum failure
        // after the whole transfer has run.
        let identity = stream_identity(header);
        if self.stream != Some(identity) {
            match Self::start(header) {
                Ok(decoder) => {
                    self.budget = frame_budget(header.source_blocks);
                    self.decoder = Some(decoder);
                    self.stream = Some(identity);
                }
                Err(message) => {
                    self.reset();
                    return ReceiverEvent::Incompatible { message };
                }
            }
        }

        let decoder = self
            .decoder
            .as_mut()
            .expect("a locked stream always has a decoder");
        if decoder.add_frame(header.seq, parsed.block).is_err() {
            // Unreachable while stream identity pins `block_len`, but a decoder
            // that cannot take the frame must not be asked about it either.
            return ReceiverEvent::Ignored;
        }

        if decoder.is_complete() {
            let container = match decoder.assemble() {
                Ok(container) => container,
                Err(_) => {
                    self.reset();
                    return ReceiverEvent::Failed {
                        message: "The transfer reported itself complete but could not be \
                                  assembled."
                            .to_owned(),
                    };
                }
            };
            self.reset();
            return finish(&container, header);
        }

        if decoder.unique_frames() >= self.budget {
            let seen = decoder.unique_frames();
            self.reset();
            return ReceiverEvent::Failed {
                message: format!(
                    "Gave up after {seen} frames without recovering the file. \
                     Restart the sender and try again."
                ),
            };
        }

        ReceiverEvent::Progress {
            unique_frames: decoder.unique_frames(),
            solved_blocks: decoder.solved_count(),
            total_blocks: usize::from(header.source_blocks),
        }
    }

    /// Build a decoder for a header, or say why these dimensions are refused.
    fn start(header: FrameHeader) -> Result<LtDecoder, String> {
        LtDecoder::new(
            header.source_blocks,
            usize::from(header.block_len),
            header.session_id,
            header.total_len as usize,
        )
        .map_err(|error| match error {
            OpticalError::UnsupportedDimensions => format!(
                "That stream describes a {} byte transfer in {} blocks of {} bytes, \
                 which this build will not decode.",
                header.total_len, header.source_blocks, header.block_len
            ),
            other => other.to_string(),
        })
    }
}

/// Turn an assembled container into a verified result, or a failure.
///
/// Both checks are independent and both must pass. The frame FNV catches a
/// stream assembled from frames that did not belong together; the container
/// SHA-256 catches a container whose own bytes are wrong. Neither is skipped
/// because the other passed.
fn finish(container: &[u8], header: FrameHeader) -> ReceiverEvent {
    if fnv1a(container) != header.payload_fnv {
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
/// `optical-core` keeps the sender's bytes as they were, deliberately: it
/// mirrors upstream and is not the trust boundary. This is.
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
