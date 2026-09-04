# Rust Optical Transfer Design

Date: 2026-09-04

## Context

This project ports the current Decimen Optical Transfer implementation at
`v0.5.3` (`5301fc2020716b5a97d4edf6e1793c3dedbf6e17`) from TypeScript to a
shared Rust core. It keeps the proven optical protocol and algorithms while
providing a native desktop sender and an installable phone receiver.

The port is a derivative of AGPL-3.0-or-later software. The new project will
use AGPL-3.0-or-later, preserve upstream attribution, include the upstream
notices, and make complete corresponding source available with distributed
builds.

The existing Python LAN-transfer application remains untouched. The Rust
project lives under `optical-transfer/`.

## Goals

- Send a file or text snippet from a Windows, macOS, or Linux desktop by
  displaying an endless animated QR stream.
- Receive that stream with a phone camera in an installable PWA, reconstruct
  the payload despite dropped or reordered frames, verify it, and save or show
  it locally.
- Keep sender and receiver compatible through one shared Rust codec compiled
  natively and to WebAssembly.
- Match Decimen v0.5.3's wire-v3 framing, container, fountain composition, and
  validation behavior exactly enough to reuse its golden vectors.
- Work without a network path between sender and receiver during transfer.
- Preserve Unicode filenames and media types.
- Produce self-contained release artifacts for desktop targets and a static,
  offline-capable receiver bundle.

## Non-goals

- Interoperability with future Decimen releases beyond the pinned v0.5.3
  behavior.
- Native iOS or Android applications in the first release.
- Encryption or confidentiality. Anyone with a camera pointed at the sender
  can read the transfer.
- Bidirectional negotiation, retransmission, pairing, accounts, cloud storage,
  or network transfer.
- Animation export, multi-language UI, diagnostics collection, and benchmark
  record tooling from the upstream web application.

## Architecture

The project is a Cargo workspace with one protocol implementation and two thin
platform adapters:

```text
optical-transfer/
  Cargo.toml
  LICENSE
  NOTICE.md
  crates/
    optical-core/       protocol, container, fountain encoder/decoder
    optical-wasm/       wasm-bindgen API over optical-core
  apps/
    desktop-sender/     eframe/egui native sender
  web/
    receiver/           HTML, CSS, JavaScript, PWA manifest/service worker
  vendor/
    decimen-codec/      pinned QR decoder JS/WASM and license notices
  tests/
    vectors/            upstream-derived immutable conformance fixtures
```

`optical-core` has no UI or browser dependencies. It owns every byte that
crosses the optical link. `desktop-sender` handles file selection, settings,
QR generation, and display. `optical-wasm` exposes a small stateful decoder API
to the receiver. The receiver JavaScript owns camera capture and worker
scheduling because those are browser APIs, while the existing pinned
`decimen-codec` WASM performs QR recognition.

The first release deliberately does not rewrite ZXing in Rust. The decoder is
already a tuned QR-only zxing-cpp WASM build with tracked-region support. It is
portable across modern phone browsers and avoids introducing a second risky
port alongside the fountain/protocol port.

## Shared Core

### Wire frame

The Rust implementation preserves wire version 3 exactly. Every QR payload is
a 22-byte little-endian header followed by one fountain block:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | `u8` | magic `0xD1` |
| 1 | `u8` | magic `0xC3` |
| 2 | `u8` | wire version `3` |
| 3 | `u8` | feature flags |
| 4 | `u16` | session ID |
| 6 | `u32` | sequence number |
| 10 | `u16` | source block count |
| 12 | `u16` | block length |
| 14 | `u32` | total container length |
| 18 | `u32` | FNV-1a container checksum |
| 22 | bytes | encoded fountain block |

Critical flags are the low nibble and unknown critical bits reject a stream.
Unknown high-nibble flags are ignored and excluded from stream identity. Frame
classification distinguishes foreign QR data, legacy versions, newer
versions, unsupported flags, malformed frames, and valid frames before the
decoder sees a block.

### Fountain code

The sender emits a repeating `2 * K` carousel:

1. The first `K` positions emit source blocks systematically.
2. The next `K` positions emit repair blocks formed by XORing a deterministic
   subset of 4 to 24 source blocks.
3. Repair composition is seeded from session ID and absolute sequence number,
   so later cycles provide new repair combinations.

The Rust port preserves JavaScript's 32-bit wrapping integer behavior,
`splitmix32`, seed derivation, modulo operations, subset insertion order, and
little-endian word XOR semantics. The legacy robust-soliton functions and
deterministic logarithm remain in the core because they are part of upstream's
pinned compatibility surface, even though wire v3 emits the systematic
carousel.

The decoder deduplicates sequence numbers, reduces each received equation by
already-solved blocks, indexes pending equations by source block, and peels
new degree-one equations until no more progress is possible. It enforces
bounds before allocation.

### File and snippet container

The `DCF2` container remains byte-compatible with v0.5.3:

- sanitized UTF-8 basename and media type;
- original and transmitted lengths;
- SHA-256 of original bytes;
- optional gzip payload, used only when the upstream policy predicts benefit
  and compression saves more than 64 bytes;
- 64 MiB maximum original file size;
- bounded decompression that aborts before output exceeds the declared and
  configured maximum.

Text snippets use the upstream snippet media type and limits. The receiver
shows snippet text directly and offers files only after the FNV container check
and SHA-256 file check both pass.

## Desktop Sender

The desktop sender uses `eframe/egui` to provide one native codebase for
Windows, macOS, and Linux. The UI has two modes:

- File: choose a file, inspect its name/size/compression result, then start.
- Text: paste text, then start.

The transfer view renders one or more standards-compliant QR codes against a
high-contrast background. Initial settings expose only the useful upstream
controls: frame payload, frame rate, error-correction level, grid count, and
display size. Defaults favor reliable phone scanning rather than maximum
throughput. Changing any transfer-defining setting starts a new random nonzero
session ID and resets the sequence counter.

QR generation uses a Rust QR encoder that supports binary segments, explicit
version selection, error-correction level, and a fixed mask. Frames are
prepared slightly ahead of display so encoding does not disturb frame timing.
The app requests continuous repaint while streaming and prevents stale
generation work from replacing frames after a transfer is stopped or changed.

Release targets are:

- `x86_64-pc-windows-msvc`;
- `x86_64-unknown-linux-gnu`;
- `x86_64-apple-darwin`;
- `aarch64-apple-darwin`.

The two macOS binaries may additionally be combined into a universal binary in
release automation. Platform installers are not required initially; zipped
executables/app bundles and checksums are sufficient.

## Phone Receiver

The receiver is a framework-free static PWA. It requires HTTPS for initial
camera access, can be installed to the home screen, and works offline after its
assets have been cached successfully.

The browser layer:

1. Requests the rear camera with practical width and frame-rate constraints.
2. Captures frames using `requestVideoFrameCallback` where available.
3. Sends full frames or tracked crops to a small Web Worker pool.
4. Uses the pinned Decimen QR decoder WASM to return binary QR payloads and
   detected regions.
5. Passes decoded payloads into `optical-wasm`.
6. Updates progress from unique frames collected and source blocks solved.
7. On completion, verifies and exposes the recovered payload locally.

Busy workers drop camera frames instead of building a queue. Fountain coding
makes those frames disposable, while an image queue would increase latency and
heat. Region tracking, periodic full-frame reacquisition, camera generation
counters, and stream-identity resets retain the upstream receiver behavior
needed for real phones.

The PWA stores no received payload persistently unless the user explicitly
saves it. Object URLs are revoked when replaced. A text result is rendered as
text, never interpreted as HTML.

## Data Flow

### Send

1. User selects a file or enters text.
2. Sender sanitizes metadata, computes SHA-256, optionally compresses, and
   constructs the container.
3. Sender chooses a valid block length for the requested QR capacity and
   creates the fountain encoder.
4. For each sequence number, the core returns an encoded block and packed v3
   frame.
5. The desktop adapter turns the frame bytes into QR modules and displays them
   at the configured cadence indefinitely.

### Receive

1. Camera worker decodes one or more binary QR symbols.
2. Core classifies and parses each frame.
3. A changed stream identity resets receiver state; duplicate sequences are
   ignored.
4. Fountain equations are reduced and peeled until all source blocks resolve.
5. Reassembled bytes are truncated to `totalLen` and checked against
   `payloadFnv`.
6. Container metadata and lengths are validated before decompression.
7. Original bytes are checked against SHA-256.
8. Verified text is shown or a verified file is offered for download/preview.

## Error Handling and Safety

- All frame and container lengths are validated before indexing or allocating.
- Maximum source blocks, frame size, and recovered bytes have explicit bounds.
- Decompression has a hard output ceiling to prevent gzip bombs.
- Filenames are reduced to safe basenames and stripped of control characters
  on both sender and receiver.
- Invalid or unrelated QR codes are silently ignored. Recognized incompatible
  versions and critical flags produce actionable errors.
- Decoder panics do not cross the WASM boundary; errors become stable tagged
  results for the UI.
- A checksum failure never produces a downloadable file.
- The UI clearly states that optical transfer is not encrypted.

## Testing

Development starts with conformance tests before application code.

### Core conformance

- Translate upstream protocol, fountain, frame-capacity, snippet, and transfer
  vectors into Rust fixtures.
- Assert the canonical v3 frame byte-for-byte.
- Assert every frame-classification verdict and malformed boundary.
- Assert exact fountain compositions for pinned sessions and sequences,
  including 32-bit wrapping behavior.
- Run a deterministic end-to-end 300 KiB transfer with approximately 15%
  frame loss and require recovery below the upstream 1.3x overhead bound.
- Test Unicode filenames, media types, gzip selection, bounded inflate,
  checksum failures, duplicates, reordered frames, and stream resets.

### Cross-build compatibility

- Run the same fixtures against native `optical-core` and WASM
  `optical-wasm`.
- During porting, use the pinned upstream v0.5.3 implementation as an oracle to
  generate comparison fixtures; it is not a runtime dependency.
- Encode deterministic native frames and decode them through the WASM core,
  then perform the reverse container checks.

### Application verification

- Compile and test the desktop sender on the four release targets in CI.
- Build the receiver with no network-loaded runtime assets.
- Browser smoke tests cover service-worker installation, offline reload,
  camera-error messaging, worker startup, completed text, and completed file
  download.
- Manual hardware acceptance covers macOS sender to iPhone and Android, plus
  one Windows or Linux sender, at both default and reduced-density settings.

## Acceptance Criteria

- The native sender displays a file or text stream continuously on Windows,
  macOS, and Linux builds.
- An installed phone PWA can receive after the network is disabled.
- A receiver may begin mid-stream and still complete.
- At least 15% deterministic frame loss does not corrupt or prevent the test
  transfer within the defined overhead bound.
- Unicode filenames and arbitrary binary files round-trip exactly.
- The receiver never exposes bytes before both integrity checks pass.
- Native and WASM codec outputs match every pinned golden vector.
- Release archives include binaries/static assets, AGPL source offer,
  attribution, license notices, and SHA-256 checksums.

## Delivery Sequence

1. Establish workspace, licensing, upstream snapshot, and golden fixtures.
2. Port and verify `optical-core` without UI.
3. Add the WASM boundary and prove native/WASM fixture parity.
4. Build the minimal native desktop sender.
5. Integrate the existing QR decoder into the phone PWA receiver.
6. Run browser and hardware transfer verification.
7. Add cross-platform release builds and documentation.

