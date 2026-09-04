//! The receiver has to behave identically whether it is driven from a browser
//! or from a test binary, so every assertion here runs against the plain Rust
//! `ReceiverState`. The `wasm_bindgen` wrapper only serialises what this type
//! returns; if these pass, a phone sees the same answers.

use optical_core::{
    container::pack_file,
    fountain::LtEncoder,
    protocol::{FrameHeader, SplitMix32, fnv1a, pack_frame},
    snippet::pack_snippet,
};
use optical_wasm::{ReceiverEvent, ReceiverState, frame_budget};

const BLOCK_LEN: usize = 1445;
const SESSION: u16 = 0x4d2;

/// A deterministic sender for one container: hands out wire frames on demand.
struct Sender {
    encoder: LtEncoder,
    header: FrameHeader,
}

impl Sender {
    fn new(container: &[u8]) -> Self {
        let encoder = LtEncoder::new(container, BLOCK_LEN, SESSION).unwrap();
        let header = FrameHeader {
            session_id: SESSION,
            seq: 0,
            source_blocks: encoder.source_blocks(),
            block_len: BLOCK_LEN as u16,
            total_len: container.len() as u32,
            payload_fnv: fnv1a(container),
            flags: 0,
        };
        Self { encoder, header }
    }

    fn source_blocks(&self) -> u16 {
        self.encoder.source_blocks()
    }

    fn frame(&self, seq: u32) -> Vec<u8> {
        self.frame_with(
            FrameHeader { seq, ..self.header },
            &self.encoder.encode(seq),
        )
    }

    fn frame_with(&self, header: FrameHeader, block: &[u8]) -> Vec<u8> {
        pack_frame(header, block).unwrap()
    }
}

fn long_text() -> String {
    (0..900)
        .map(|line| format!("line {line}: optical transfer parity — 光学传输\n"))
        .collect()
}

/// Incompressible bytes, so the container's block count reflects its size.
fn noise(length: usize) -> Vec<u8> {
    let mut random = SplitMix32::new(0x51ed_2701);
    (0..length).map(|_| random.next_u32() as u8).collect()
}

#[test]
fn a_lossy_snippet_stream_completes_with_the_bytes_that_were_sent() {
    let text = long_text();
    let packed = pack_snippet(&text).unwrap();
    let sender = Sender::new(&packed.container);
    let mut receiver = ReceiverState::new();

    let mut completed = None;
    for seq in 0..20_000_u32 {
        // Every seventh frame never reaches the camera.
        if seq % 7 == 6 {
            continue;
        }
        if let ReceiverEvent::Complete {
            name,
            media_type,
            bytes,
            is_text,
        } = receiver.add_frame(&sender.frame(seq))
        {
            completed = Some((name, media_type, bytes, is_text));
            break;
        }
    }

    let (name, media_type, bytes, is_text) = completed.expect("stream never completed");
    assert_eq!(name, "snippet.txt");
    assert_eq!(media_type, "application/vnd.decimen.snippet");
    assert!(is_text);
    assert_eq!(String::from_utf8(bytes).unwrap(), text);
}

#[test]
fn a_lossy_binary_stream_completes_with_the_bytes_that_were_sent() {
    let payload = noise(200 * 1024);
    let packed = pack_file("photo.bin", "application/octet-stream", &payload).unwrap();
    let sender = Sender::new(&packed.container);
    let mut receiver = ReceiverState::new();

    let mut completed = None;
    for seq in 0..40_000_u32 {
        if seq % 7 == 6 {
            continue;
        }
        if let ReceiverEvent::Complete {
            name,
            media_type,
            bytes,
            is_text,
        } = receiver.add_frame(&sender.frame(seq))
        {
            completed = Some((name, media_type, bytes, is_text));
            break;
        }
    }

    let (name, media_type, bytes, is_text) = completed.expect("stream never completed");
    assert_eq!(name, "photo.bin");
    assert_eq!(media_type, "application/octet-stream");
    assert!(!is_text);
    assert_eq!(bytes, payload);
}

#[test]
fn a_foreign_qr_code_is_ignored_without_touching_stream_state() {
    let mut receiver = ReceiverState::new();
    assert!(matches!(
        receiver.add_frame(b"https://example.invalid/not-a-decimen-frame"),
        ReceiverEvent::Ignored
    ));
    assert!(matches!(receiver.add_frame(&[]), ReceiverEvent::Ignored));
    // Ours, well-formed magic and version, but no payload: malformed is silent
    // for the same reason foreign is — the camera sees partial reads all day.
    assert!(matches!(
        receiver.add_frame(&[0xd1, 0xc3, 3, 0]),
        ReceiverEvent::Ignored
    ));
}

#[test]
fn a_sender_this_build_cannot_read_says_so_instead_of_going_quiet() {
    let mut receiver = ReceiverState::new();
    let ReceiverEvent::Incompatible { message } = receiver.add_frame(&[0xd1, 0xc3, 4, 0]) else {
        panic!("a newer wire version must be named, not ignored");
    };
    assert!(
        message.contains('4'),
        "the message must name the version: {message}"
    );

    assert!(matches!(
        receiver.add_frame(&[0xd1, 0x0c, 3, 0]),
        ReceiverEvent::Incompatible { .. }
    ));
    assert!(matches!(
        receiver.add_frame(&[0xd1, 0xc3, 3, 1]),
        ReceiverEvent::Incompatible { .. }
    ));
}

#[test]
fn progress_reports_the_stream_the_receiver_actually_locked_onto() {
    let packed = pack_snippet(&long_text()).unwrap();
    let sender = Sender::new(&packed.container);
    let mut receiver = ReceiverState::new();

    let ReceiverEvent::Progress {
        unique_frames,
        solved_blocks,
        total_blocks,
    } = receiver.add_frame(&sender.frame(0))
    else {
        panic!("the first frame of a fresh stream is progress");
    };
    assert_eq!(unique_frames, 1);
    assert_eq!(solved_blocks, 1);
    assert_eq!(total_blocks, packed.container.len().div_ceil(BLOCK_LEN));

    // A repeat of a frame already seen is progress, not new information.
    let ReceiverEvent::Progress { unique_frames, .. } = receiver.add_frame(&sender.frame(0)) else {
        panic!("a duplicate frame is still progress");
    };
    assert_eq!(unique_frames, 1);
}

#[test]
fn a_stream_whose_checksum_disagrees_never_reaches_the_caller() {
    let packed = pack_snippet(&long_text()).unwrap();
    let sender = Sender::new(&packed.container);
    // A header whose FNV describes a payload the sender is not sending. The
    // blocks are perfectly recoverable; the claim about them is the lie.
    let header = FrameHeader {
        payload_fnv: fnv1a(&packed.container) ^ 0xffff_ffff,
        ..sender.header
    };
    let mut receiver = ReceiverState::new();
    let mut event = ReceiverEvent::Ignored;
    for seq in 0..u32::from(sender.source_blocks()) {
        event = receiver.add_frame(
            &sender.frame_with(FrameHeader { seq, ..header }, &sender.encoder.encode(seq)),
        );
    }
    assert!(
        matches!(event, ReceiverEvent::Failed { .. }),
        "a stream-checksum mismatch must fail, not complete: {event:?}"
    );
}

#[test]
fn a_new_stream_replaces_the_one_before_it() {
    let first =
        pack_snippet(&"the first transfer, long enough to need blocks. ".repeat(200)).unwrap();
    let second = pack_file("second.bin", "application/octet-stream", &noise(40 * 1024)).unwrap();
    let first_sender = Sender::new(&first.container);
    let second_sender = Sender::new(&second.container);

    let mut receiver = ReceiverState::new();
    receiver.add_frame(&first_sender.frame(0));
    receiver.add_frame(&first_sender.frame(1));

    let ReceiverEvent::Progress { unique_frames, .. } = receiver.add_frame(&second_sender.frame(0))
    else {
        panic!("the first frame of the new stream is progress");
    };
    assert_eq!(
        unique_frames, 1,
        "the previous stream's frames must not be counted against the new one"
    );
}

#[test]
fn a_stream_that_will_not_converge_is_abandoned_rather_than_grown() {
    // Individually valid repair frames carrying blocks that XOR to nothing
    // useful. Each one is a fresh multi-block equation, so a decoder that keeps
    // every one of them has no bound on its memory at all. The receiver gives a
    // stream a generous multiple of its block count and then gives up.
    let packed = pack_file("noise.bin", "application/octet-stream", &noise(90 * 1024)).unwrap();
    let sender = Sender::new(&packed.container);
    let k = u32::from(sender.source_blocks());
    assert!(k > 24, "the repair degree must exceed 1 for this to bind");
    let budget = frame_budget(sender.source_blocks());

    let mut receiver = ReceiverState::new();
    let mut abandoned_at = None;
    for attempt in 0..(budget as u32 * 2) {
        // Always past the systematic sweep, so nothing is ever solved outright.
        let seq = attempt
            .wrapping_mul(2 * k)
            .wrapping_add(k)
            .wrapping_add(attempt % k);
        let event = receiver.add_frame(&sender.frame_with(
            FrameHeader {
                seq,
                ..sender.header
            },
            &vec![0xa5; BLOCK_LEN],
        ));
        if matches!(event, ReceiverEvent::Failed { .. }) {
            abandoned_at = Some(attempt as usize);
            break;
        }
    }
    let abandoned_at = abandoned_at.expect("the receiver never stopped accepting frames");
    assert!(
        abandoned_at <= budget,
        "gave up after {abandoned_at} frames, budget was {budget}"
    );

    // And it is ready for a real stream immediately afterwards.
    assert!(matches!(
        receiver.add_frame(&sender.frame(0)),
        ReceiverEvent::Progress {
            unique_frames: 1,
            ..
        }
    ));
}
