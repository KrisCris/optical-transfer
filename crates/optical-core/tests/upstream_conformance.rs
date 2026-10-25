//! Conformance against bytes this repository did not produce.
//!
//! Every other test in this crate pins a constant transcribed from upstream, or
//! round-trips the Rust implementation against itself. Both can be
//! self-consistently wrong. These vectors were emitted by the pinned upstream
//! source itself -- Decimen Optical Transfer v0.5.3, commit
//! 5301fc2020716b5a97d4edf6e1793c3dedbf6e17 -- running its own
//! `shared/protocol.ts`, `shared/snippet.ts` and `shared/fountain.ts`, so the
//! SHA-256 came from `crypto.subtle` and the gzip from `CompressionStream`.
//!
//! That last part is the one that could not be reasoned about from the source:
//! `flate2` and `CompressionStream` are different deflate implementations and
//! will not agree byte for byte on what they *emit*. Interoperability depends
//! on each being able to read the other's output, which is a claim only a real
//! cross-implementation vector can settle.
//!
//! To regenerate after a baseline bump, run the emitter against a checkout of
//! the new upstream tag; a diff here is a wire-format change, not a constant to
//! re-record.

use std::collections::HashMap;

use optical_core::{
    container::{Compression, unpack_file, verify_file},
    fountain::LtDecoder,
    protocol::{FrameVerdict, Wire, classify_frame, fnv1a, parse_frame, stream_identity},
    receiver::{ReceiverEvent, ReceiverState},
    snippet::{is_snippet, snippet_text},
};

const VECTORS: &str = include_str!("vectors/upstream-v0.5.3.txt");

struct Case {
    id: String,
    fields: HashMap<String, String>,
    frames: Vec<Vec<u8>>,
}

impl Case {
    fn text(&self, key: &str) -> &str {
        self.fields
            .get(key)
            .unwrap_or_else(|| panic!("case {} has no {key}", self.id))
    }

    fn bytes(&self, key: &str) -> Vec<u8> {
        unhex(self.text(key))
    }

    fn number(&self, key: &str) -> usize {
        self.text(key).parse().expect("numeric field")
    }
}

fn unhex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2), "odd-length hex in vectors");
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).expect("hex byte"))
        .collect()
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let mut current: Option<Case> = None;
    for line in VECTORS.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "case" => {
                current = Some(Case {
                    id: value.to_owned(),
                    fields: HashMap::new(),
                    frames: Vec::new(),
                });
            }
            "end" => cases.push(current.take().expect("end outside a case")),
            _ => {
                let case = current.as_mut().expect("field outside a case");
                if key == "frame" {
                    case.frames.push(unhex(value));
                } else {
                    case.fields.insert(key.to_owned(), value.to_owned());
                }
            }
        }
    }
    assert!(current.is_none(), "unterminated case in vectors");
    assert!(!cases.is_empty(), "no vectors loaded");
    cases
}

#[test]
fn browser_written_containers_unpack_and_verify() {
    let cases = cases();
    assert_eq!(cases.len(), 4, "the vector file lost a case");
    for case in &cases {
        let container = case.bytes("container");
        let expected = case.bytes("original");

        let file = unpack_file(&container)
            .unwrap_or_else(|error| panic!("case {}: unpack failed: {error}", case.id));
        assert!(
            verify_file(&file),
            "case {}: SHA-256 written by crypto.subtle did not verify",
            case.id
        );
        assert_eq!(file.name, case.text("name"), "case {}: name", case.id);
        assert_eq!(
            file.media_type,
            case.text("type"),
            "case {}: media type",
            case.id
        );
        assert_eq!(
            file.bytes, expected,
            "case {}: recovered bytes differ from what the sender packed",
            case.id
        );

        let expected_compression = match case.text("compression") {
            "gzip" => Compression::Gzip,
            "none" => Compression::None,
            other => panic!("case {}: unknown compression {other}", case.id),
        };
        assert_eq!(
            file.compression, expected_compression,
            "case {}: the compression decision itself is part of the contract",
            case.id
        );
    }

    // The point of the exercise: at least one case must have arrived gzipped,
    // or nothing here tested cross-implementation inflate at all.
    assert!(
        cases.iter().any(|case| case.text("compression") == "gzip"),
        "the vectors no longer cover browser-produced gzip"
    );
}

#[test]
fn a_browser_written_lossy_frame_stream_decodes_to_the_same_text() {
    let case = cases()
        .into_iter()
        .find(|case| case.id == "snippet-stream")
        .expect("the streaming case is missing");
    assert!(!case.frames.is_empty());

    let mut decoder = None;
    let mut identity = None;
    let mut expected_fnv = 0;

    for frame in &case.frames {
        assert_eq!(
            classify_frame(frame),
            FrameVerdict::Valid(Wire::V3),
            "case {}: a frame packed by upstream must classify as ours",
            case.id
        );
        let parsed = parse_frame(frame).unwrap().expect("valid frames parse");
        let header = parsed.header;
        assert_eq!(usize::from(header.source_blocks), case.number("k"));
        assert_eq!(usize::from(header.block_len), case.number("blockLen"));
        assert_eq!(usize::from(header.session_id), case.number("sessionId"));

        let current = stream_identity(header);
        if identity != Some(current) {
            identity = Some(current);
            expected_fnv = header.payload_fnv;
            decoder = Some(
                LtDecoder::new(
                    header.source_blocks,
                    usize::from(header.block_len),
                    header.session_id,
                    header.total_len as usize,
                )
                .expect("upstream dimensions must satisfy the Rust decoder's bounds"),
            );
        }
        decoder
            .as_mut()
            .unwrap()
            .add_frame(header.seq, parsed.block)
            .unwrap();
        if decoder.as_ref().unwrap().is_complete() {
            break;
        }
    }

    let decoder = decoder.expect("frames were present");
    assert!(
        decoder.is_complete(),
        "the upstream carousel did not complete in the Rust decoder"
    );
    let container = decoder.assemble().unwrap();
    assert_eq!(
        fnv1a(&container),
        expected_fnv,
        "the assembled container disagrees with the FNV upstream computed for it"
    );
    assert_eq!(container, case.bytes("container"));

    let file = unpack_file(&container).unwrap();
    assert!(verify_file(&file));
    assert!(
        is_snippet(&file),
        "upstream's snippet media type must be recognised"
    );
    assert_eq!(
        snippet_text(&file).unwrap().as_bytes(),
        case.bytes("original")
    );
}

#[test]
fn the_receiver_still_reads_an_upstream_stream_end_to_end() {
    // This build's senders moved to wire v4; its receivers did not leave v3
    // behind. The same upstream-written frames, through the state machine both
    // the browser and the desktop receiver run.
    let case = cases()
        .into_iter()
        .find(|case| case.id == "snippet-stream")
        .expect("the streaming case is missing");
    let mut receiver = ReceiverState::new();
    for frame in &case.frames {
        match receiver.add_frame(frame) {
            ReceiverEvent::Progress { stats, .. } => assert_eq!(stats.wire, 3),
            ReceiverEvent::Complete {
                bytes,
                is_text,
                stats,
                ..
            } => {
                assert_eq!(stats.wire, 3);
                assert!(is_text);
                assert_eq!(bytes, case.bytes("original"));
                return;
            }
            other => panic!("case {}: unexpected {other:?}", case.id),
        }
    }
    panic!("the upstream stream did not complete in the receiver");
}
