# Rust Optical Transfer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a native Rust desktop sender and an offline-capable phone PWA receiver that transfer files and text through fountain-coded animated QR frames.

**Architecture:** A Cargo workspace contains a platform-neutral `optical-core`, a `wasm-bindgen` adapter, and an `eframe` desktop sender. A framework-free PWA owns browser camera APIs and uses the pinned Decimen QR decoder WASM before passing decoded frame bytes into the shared Rust decoder.

**Tech Stack:** Rust 1.98.1 with edition 2024, Cargo, `sha2`, `flate2`, `wasm-bindgen`, `serde`, `eframe/egui`, `qrcodegen`, `rfd`, vanilla HTML/CSS/JavaScript, Web Workers, and the pinned Decimen codec WASM.

**Spec:** `docs/superpowers/specs/2026-09-04-optical-transfer-rust-design.md`

## Global Constraints

- Source baseline is Decimen Optical Transfer `v0.5.3`, commit `5301fc2020716b5a97d4edf6e1793c3dedbf6e17`.
- The complete project is licensed AGPL-3.0-or-later and preserves upstream attribution and third-party notices.
- The existing Python LAN-transfer application outside this repository is not modified.
- Wire version 3, its 22-byte header, DCF2 container, systematic-carousel fountain composition, and golden vectors are compatibility contracts.
- Maximum original file size is exactly 64 MiB.
- The first release targets Windows x86-64, Linux x86-64, macOS x86-64, macOS arm64, and modern iOS/Android browsers.
- The phone receiver is a PWA, not a native mobile application.
- No runtime asset may require an internet connection after successful PWA installation.
- Text is rendered as text and never interpreted as HTML.
- Received bytes are not exposed until FNV-1a and SHA-256 verification both succeed.
- The optical channel is not encrypted and the UI states that clearly.
- Dependency versions are committed in `Cargo.lock`; workspace crates are not published to crates.io.

---

### Task 1: Reproducible Workspace and Licensing

**Files:**

- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `LICENSE`
- Create: `NOTICE.md`
- Create: `README.md`
- Create: `crates/optical-core/Cargo.toml`
- Create: `crates/optical-core/src/lib.rs`

**Interfaces:**

- Consumes: approved design and Decimen v0.5.3 license notices.
- Produces: Cargo workspace with package `optical-core` and a stable Rust toolchain declaration.

- [ ] **Step 1: Install the development toolchain if `cargo` is absent**

Run:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --locked
```

Expected: `cargo --version`, `rustc --version`, and `wasm-pack --version` all succeed.

- [ ] **Step 2: Write a failing workspace check**

Run:

```bash
test -f Cargo.toml && cargo metadata --no-deps --format-version 1
```

Expected: failure because `Cargo.toml` does not exist.

- [ ] **Step 3: Create the workspace manifests**

Create `Cargo.toml`:

```toml
[workspace]
members = [
  "crates/optical-core",
]
resolver = "3"

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.98.1"
license = "AGPL-3.0-or-later"

[profile.release]
lto = "thin"
strip = true
```

Create `rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.98.1"
profile = "minimal"
targets = ["wasm32-unknown-unknown"]
components = ["clippy", "rustfmt"]
```

Create `crates/optical-core/Cargo.toml`:

```toml
[package]
name = "optical-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
flate2 = { version = "1.1.10", default-features = false, features = ["rust_backend"] }
sha2 = "0.11.0"
```

Create `crates/optical-core/src/lib.rs`:

```rust
#![forbid(unsafe_code)]

pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
```

Create `.gitignore`:

```gitignore
/target/
/.tooling/
/web/receiver/pkg/
/dist/
.DS_Store
```

Copy the AGPL-3.0-or-later text from the pinned upstream `LICENSE`. Write
`NOTICE.md` with the upstream project name, author, v0.5.3 commit, source URL,
AGPL status, and zxing-cpp Apache-2.0 notice. Write `README.md` with the
architecture, security warning, build prerequisites, and license attribution.

- [ ] **Step 4: Verify workspace and legal files**

Run:

```bash
cargo metadata --no-deps --format-version 1
test -s LICENSE
test -s NOTICE.md
rg -n "5301fc2020716b5a97d4edf6e1793c3dedbf6e17|AGPL-3.0-or-later|zxing-cpp" NOTICE.md README.md
```

Expected: Cargo reports `optical-core` as the only workspace member and the
legal-file checks pass. Later tasks add their member only when its manifest is
created, keeping every intermediate commit buildable.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore LICENSE NOTICE.md README.md crates
git commit -m "Make the port reproducible and legally distributable" \
  -m "Pin the Rust workspace and carry the notices required by the v0.5.3 AGPL source baseline." \
  -m "Constraint: Rust 1.98.1 and wire-v3 compatibility" \
  -m "Confidence: high" \
  -m "Scope-risk: narrow" \
  -m "Tested: cargo metadata and license attribution checks" \
  -m "Not-tested: no protocol implementation exists yet"
```

---

### Task 2: Wire-v3 Framing and Deterministic Primitives

**Files:**

- Create: `crates/optical-core/src/error.rs`
- Create: `crates/optical-core/src/protocol.rs`
- Modify: `crates/optical-core/src/lib.rs`
- Create: `crates/optical-core/tests/protocol_vectors.rs`

**Interfaces:**

- Consumes: raw QR payload bytes.
- Produces: `FrameHeader`, `FrameVerdict`, `ParsedFrame`, `StreamIdentity`,
  `pack_frame`, `classify_frame`, `parse_frame`, `fnv1a`, and `SplitMix32`.

- [ ] **Step 1: Add the canonical failing frame tests**

Create `crates/optical-core/tests/protocol_vectors.rs`:

```rust
use optical_core::protocol::{
    classify_frame, pack_frame, FrameHeader, FrameVerdict, HEADER_LEN,
};

#[test]
fn canonical_wire_v3_frame_is_exact() {
    let header = FrameHeader {
        session_id: 0xbeef,
        seq: 0x0102_0304,
        source_blocks: 0x0111,
        block_len: 6,
        total_len: 0x00fe_dcba,
        payload_fnv: 0x89ab_cdef,
        flags: 0,
    };
    let actual = pack_frame(header, &[1, 2, 3, 4, 5, 6]).unwrap();
    assert_eq!(HEADER_LEN, 22);
    assert_eq!(
        actual,
        hex_bytes("d1 c3 03 00 ef be 04 03 02 01 11 01 06 00 ba dc fe 00 ef cd ab 89 01 02 03 04 05 06")
    );
}

#[test]
fn classification_distinguishes_versions_and_flags() {
    assert_eq!(classify_frame(&[0xd2, 0xc3, 3, 0]), FrameVerdict::Foreign);
    assert_eq!(classify_frame(&[0xd1, 0x0c, 3, 0]), FrameVerdict::OlderSender(1));
    assert_eq!(classify_frame(&[0xd1, 0xc3, 4, 0]), FrameVerdict::NewerSender(4));
    assert_eq!(classify_frame(&[0xd1, 0xc3, 3, 1]), FrameVerdict::UnsupportedFlags(1));
}

fn hex_bytes(value: &str) -> Vec<u8> {
    value.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
}
```

- [ ] **Step 2: Run the protocol test and confirm failure**

Run:

```bash
cargo test -p optical-core --test protocol_vectors
```

Expected: compile failure because `optical_core::protocol` does not exist.

- [ ] **Step 3: Implement protocol types and exact packing**

Create `protocol.rs` with these public definitions:

```rust
pub const HEADER_LEN: usize = 22;
pub const WIRE_VERSION: u8 = 3;
pub const CRITICAL_FLAGS: u8 = 0x0f;

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

pub struct ParsedFrame<'a> {
    pub header: FrameHeader,
    pub block: &'a [u8],
}

pub fn pack_frame(header: FrameHeader, block: &[u8]) -> Result<Vec<u8>, OpticalError>;
pub fn classify_frame(bytes: &[u8]) -> FrameVerdict;
pub fn parse_frame(bytes: &[u8]) -> Result<Option<ParsedFrame<'_>>, OpticalError>;
pub fn stream_identity(header: FrameHeader) -> StreamIdentity;
```

Implement all integer reads and writes explicitly with
`to_le_bytes`/`from_le_bytes`. `pack_frame` rejects zero dimensions, a block
length mismatch, or lengths outside the wire types. `classify_frame` follows
the upstream verdict ordering exactly. `parse_frame` returns `Ok(None)` for
non-valid frames and borrows the block without copying.

Implement deterministic primitives with wrapping arithmetic:

```rust
pub fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

pub struct SplitMix32(u32);

impl SplitMix32 {
    pub fn new(seed: u32) -> Self { Self(seed) }
    pub fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x9e37_79b9);
        let mut value = self.0 ^ (self.0 >> 16);
        value = value.wrapping_mul(0x21f0_aaad);
        value ^= value >> 15;
        value = value.wrapping_mul(0x735a_2d97);
        value ^ (value >> 15)
    }
}
```

Export modules from `lib.rs` and implement `OpticalError` in `error.rs` with
stable variants for invalid frame, unsupported dimensions, invalid container,
size limit, checksum failure, compression failure, and incomplete transfer.

- [ ] **Step 4: Add all upstream classification and stream-identity vectors**

Extend the test with the complete classification table from the design,
malformed frames shorter than 23 bytes, zero `K`, zero block length, zero total
length, exact-length mismatch, ignorable flag acceptance, critical-only stream
identity, FNV vectors, and SplitMix32 sequences copied from the pinned upstream
tests.

- [ ] **Step 5: Run formatting, tests, and lint**

Run:

```bash
cargo fmt --all -- --check
cargo test -p optical-core --test protocol_vectors
cargo clippy -p optical-core --all-targets -- -D warnings
```

Expected: all commands succeed.

- [ ] **Step 6: Commit**

```bash
git add crates/optical-core
git commit -m "Keep native and browser builds on one wire contract" \
  -m "Port wire-v3 framing and deterministic integer primitives against the upstream golden vectors." \
  -m "Constraint: Every multibyte field is little-endian and byte-exact" \
  -m "Rejected: Serde-based frame encoding | implicit layout cannot serve as a wire contract" \
  -m "Confidence: high" \
  -m "Scope-risk: moderate" \
  -m "Directive: A vector change requires a wire-version decision" \
  -m "Tested: protocol vectors, cargo test, rustfmt, clippy"
```

---

### Task 3: Systematic-Carousel Fountain Codec

**Files:**

- Create: `crates/optical-core/src/fountain.rs`
- Modify: `crates/optical-core/src/lib.rs`
- Create: `crates/optical-core/tests/fountain_vectors.rs`

**Interfaces:**

- Consumes: container bytes, block length, session ID, sequence-numbered
  blocks.
- Produces: `cycle_length`, `frame_composition`, `LtEncoder::new`,
  `LtEncoder::encode`, `LtDecoder::new`, `LtDecoder::add_frame`,
  `LtDecoder::is_complete`, and `LtDecoder::assemble`.

- [ ] **Step 1: Write failing composition and stream-fingerprint tests**

Create tests that assert:

```rust
#[test]
fn sweep_is_systematic_and_cycles() {
    for k in [1_u16, 2, 17, 179] {
        for position in 0..k {
            assert_eq!(frame_composition(k, 9, u32::from(position)), vec![position]);
            assert_eq!(
                frame_composition(k, 9, u32::from(position) + 6 * cycle_length(k)),
                vec![position]
            );
        }
    }
}

#[test]
fn encoded_stream_fingerprints_match_upstream() {
    let cases = [
        (1, 64, 1, 0xf6a1_15c5),
        (23, 64, 7, 0x4a5d_3eaa),
        (179, 2933, 4242, 0x54f7_8d05),
        (716, 1445, 65535, 0x75b7_3b85),
    ];
    for (k, block_len, session, expected) in cases {
        let payload = test_payload(k * block_len - 7);
        let encoder = LtEncoder::new(&payload, block_len, session).unwrap();
        let stream: Vec<u8> = (0..64).flat_map(|seq| encoder.encode(seq)).collect();
        assert_eq!(fnv1a(&stream), expected);
    }
}
```

Use `test_payload(len)` where byte `i` is
`(i * 37 + (i >> 8) * 11) & 0xff` with wrapping integer operations.

- [ ] **Step 2: Run tests and confirm missing fountain API**

Run:

```bash
cargo test -p optical-core --test fountain_vectors
```

Expected: compile failure for unresolved fountain symbols.

- [ ] **Step 3: Implement exact frame composition and encoder**

Use these signatures:

```rust
pub fn cycle_length(k: u16) -> u32;
pub fn frame_composition(k: u16, session_id: u16, seq: u32) -> Vec<u16>;

pub struct LtEncoder {
    k: u16,
    block_len: usize,
    session_id: u16,
    blocks: Vec<u32>,
    words_per_block: usize,
}

impl LtEncoder {
    pub fn new(payload: &[u8], block_len: usize, session_id: u16) -> Result<Self, OpticalError>;
    pub fn source_blocks(&self) -> u16;
    pub fn encode(&self, seq: u32) -> Vec<u8>;
}
```

Port `frame_seed` with `wrapping_add`, `wrapping_mul`, XOR, and logical shifts.
Repair degree is `min(K, 4 + random % 21)`. Preserve insertion order while
deduplicating selected indices; do not use unordered-set iteration to produce
the output. Store padded blocks as little-endian `u32` words and return exactly
`block_len` bytes for every frame.

- [ ] **Step 4: Implement peeling decoder**

Use this public result and API:

```rust
pub enum AddFrame {
    Accepted,
    Duplicate,
    Redundant,
    Complete,
}

impl LtDecoder {
    pub fn new(k: u16, block_len: usize, session_id: u16, total_len: usize)
        -> Result<Self, OpticalError>;
    pub fn add_frame(&mut self, seq: u32, block: &[u8]) -> Result<AddFrame, OpticalError>;
    pub fn solved_count(&self) -> usize;
    pub fn unique_frames(&self) -> usize;
    pub fn redundant_frames(&self) -> usize;
    pub fn is_complete(&self) -> bool;
    pub fn assemble(&self) -> Result<Vec<u8>, OpticalError>;
}
```

Represent pending equations with sorted/deduplicated block indices and word
buffers. Index equation IDs by each unsolved block. When a block is solved,
XOR it from every dependent equation; newly degree-one equations enter a work
queue. Remove equations once solved or redundant so memory remains bounded.

- [ ] **Step 5: Port legacy deterministic-log vectors**

Port `dlog`, `soliton_cdf`, and `frame_indices` with the upstream constants and
exact operation ordering. Assert the 11 recorded `dlog` values, exhaustive
`0x27b0f3cc` fingerprint, CDF fingerprints, and recorded frame-index subsets.
Use `f64::to_le_bytes` when hashing CDF values.

- [ ] **Step 6: Add lossy round-trip tests**

Add clean-sweep, duplicate, redundant-frame, reordered-frame, 30% loss, and
completion tests. The 512 KiB/30% test requires exact recovered bytes,
wall-clock ratio below 2.8, and unique-frame overhead below 1.8. Add the full
300 KiB framed transfer with deterministic 15% loss and overhead below 1.3.

- [ ] **Step 7: Verify and commit**

Run:

```bash
cargo fmt --all -- --check
cargo test -p optical-core --test fountain_vectors
cargo test -p optical-core
cargo clippy -p optical-core --all-targets -- -D warnings
```

Then commit:

```bash
git add crates/optical-core
git commit -m "Make dropped optical frames recoverable without negotiation" \
  -m "Port the systematic carousel and peeling decoder while pinning JavaScript wrapping and floating-point behavior." \
  -m "Constraint: Sender and receiver derive compositions independently" \
  -m "Rejected: A new FEC crate | compatibility requires the pinned Decimen algorithm" \
  -m "Confidence: high" \
  -m "Scope-risk: broad" \
  -m "Directive: Never replace deterministic operations without rerunning every fingerprint" \
  -m "Tested: golden fingerprints, clean and lossy round trips, clippy"
```

---

### Task 4: Verified File and Text Containers

**Files:**

- Create: `crates/optical-core/src/container.rs`
- Create: `crates/optical-core/src/snippet.rs`
- Modify: `crates/optical-core/src/lib.rs`
- Create: `crates/optical-core/tests/container_vectors.rs`

**Interfaces:**

- Consumes: filename, media type, and original bytes or UTF-8 text.
- Produces: `pack_file`, `unpack_file`, `verify_file`, `pack_snippet`,
  `is_snippet`, and `snippet_text`.

- [ ] **Step 1: Write failing Unicode and integrity tests**

Create tests covering:

```rust
#[test]
fn unicode_filename_round_trips() {
    let original = "光学転送".as_bytes();
    let packed = pack_file("../報告_日本語_한글.txt", "text/plain", original).unwrap();
    let file = unpack_file(&packed.container).unwrap();
    assert_eq!(file.name, "報告_日本語_한글.txt");
    assert_eq!(file.media_type, "text/plain");
    assert_eq!(file.bytes, original);
    assert!(verify_file(&file));
}

#[test]
fn changed_payload_fails_sha256() {
    let packed = pack_file("x.bin", "application/octet-stream", &[7; 1024]).unwrap();
    let mut file = unpack_file(&packed.container).unwrap();
    file.bytes[0] ^= 1;
    assert!(!verify_file(&file));
}
```

Also assert empty input rejection, 64 MiB acceptance, 64 MiB plus one byte
rejection, malformed lengths, unknown compression, truncated gzip, gzip trailer
length mismatch, and decompression output ceiling.

- [ ] **Step 2: Confirm the tests fail**

Run:

```bash
cargo test -p optical-core --test container_vectors
```

Expected: compile failure because container functions do not exist.

- [ ] **Step 3: Implement the DCF2 container**

Use these public types:

```rust
pub enum Compression { None, Gzip }

pub struct PackedFile {
    pub container: Vec<u8>,
    pub compression: Compression,
    pub original_size: usize,
    pub transmitted_size: usize,
}

pub struct OpticalFile {
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub expected_sha256: [u8; 32],
    pub compression: Compression,
    pub transmitted_size: usize,
}

pub fn pack_file(name: &str, media_type: &str, bytes: &[u8]) -> Result<PackedFile, OpticalError>;
pub fn unpack_file(container: &[u8]) -> Result<OpticalFile, OpticalError>;
pub fn verify_file(file: &OpticalFile) -> bool;
```

Preserve the 49-byte header, `DCF2` magic, little-endian lengths, SHA-256
placement, upstream precompressed media-type policy, 768-byte compression
threshold, and 64-byte minimum saving. Use `flate2`'s Rust backend and count
inflated bytes before appending beyond the output limit.

- [ ] **Step 4: Implement text snippets**

Port the exact upstream snippet media type, byte limit, UTF-8 rules, and
container construction. Expose:

```rust
pub const MAX_SNIPPET_BYTES: usize = 64 * 1024;
pub fn pack_snippet(text: &str) -> Result<PackedFile, OpticalError>;
pub fn is_snippet(file: &OpticalFile) -> bool;
pub fn snippet_text(file: &OpticalFile) -> Result<&str, OpticalError>;
```

If the pinned upstream constant differs, use the upstream value and update the
test and public constant together.

- [ ] **Step 5: Verify and commit**

Run:

```bash
cargo fmt --all -- --check
cargo test -p optical-core
cargo clippy -p optical-core --all-targets -- -D warnings
```

Then commit:

```bash
git add crates/optical-core
git commit -m "Expose only payloads proven intact" \
  -m "Port the bounded DCF2 container, gzip policy, Unicode filename handling, snippets, and dual integrity checks." \
  -m "Constraint: Original payloads are limited to 64 MiB" \
  -m "Rejected: Trusting gzip metadata | optical input is untrusted" \
  -m "Confidence: high" \
  -m "Scope-risk: moderate" \
  -m "Directive: Keep decompression bounded before allocating output" \
  -m "Tested: container vectors, Unicode, corruption, limits, clippy"
```

---

### Task 5: WASM Decoder Boundary

**Files:**

- Create: `crates/optical-wasm/Cargo.toml`
- Create: `crates/optical-wasm/src/lib.rs`
- Create: `crates/optical-wasm/tests/web.rs`
- Create: `web/receiver/tests/decoder.html`
- Modify: `Cargo.toml`

**Interfaces:**

- Consumes: binary QR frame payloads from JavaScript.
- Produces: `WasmReceiver::new`, `WasmReceiver::reset`,
  `WasmReceiver::add_frame`, and serialized `ReceiverEvent` values.

- [ ] **Step 1: Define a failing native/WASM parity test**

Create a test that generates a deterministic packed text payload with
`optical-core`, drops every seventh frame, feeds the remaining packed frames
through the receiver-facing API, and asserts the complete event returns the
same filename, media type, and bytes. Keep receiver logic in a normal Rust
type so this test runs natively; the `wasm_bindgen` wrapper delegates to it.

- [ ] **Step 2: Add the WASM manifest and compile failure**

Create `crates/optical-wasm/Cargo.toml`:

```toml
[package]
name = "optical-wasm"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
optical-core = { path = "../optical-core" }
serde = { version = "1.0.229", features = ["derive"] }
serde-wasm-bindgen = "0.6.5"
wasm-bindgen = "0.2.127"
```

Add `"crates/optical-wasm"` to the root workspace `members` array before
running Cargo.

Run `cargo test -p optical-wasm`; expect failure because the receiver API is
not implemented.

- [ ] **Step 3: Implement the receiver state machine**

Expose a serializable event contract:

```rust
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReceiverEvent {
    Ignored,
    Incompatible { message: String },
    Progress { unique_frames: usize, solved_blocks: usize, total_blocks: usize },
    Complete { name: String, media_type: String, bytes: Vec<u8>, is_text: bool },
    Failed { message: String },
}
```

`ReceiverState::add_frame(&[u8])` classifies before parsing, resets on changed
stream identity, creates an `LtDecoder` only after validating dimensions,
checks FNV before unpacking, checks SHA-256 before returning `Complete`, and
clears decoder state after completion.

- [ ] **Step 4: Add the thin wasm-bindgen wrapper**

```rust
#[wasm_bindgen]
pub struct WasmReceiver(ReceiverState);

#[wasm_bindgen]
impl WasmReceiver {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self { Self(ReceiverState::new()) }
    pub fn reset(&mut self) { self.0.reset(); }
    pub fn add_frame(&mut self, bytes: &[u8]) -> Result<JsValue, JsValue> {
        serde_wasm_bindgen::to_value(&self.0.add_frame(bytes))
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }
}
```

- [ ] **Step 5: Build and verify both targets**

Run:

```bash
cargo test -p optical-wasm
wasm-pack build crates/optical-wasm --target web --out-dir ../../web/receiver/pkg
cargo clippy -p optical-wasm --all-targets -- -D warnings
```

Serve `web/receiver/` locally and open `tests/decoder.html`; it imports the
generated module, constructs `WasmReceiver`, and asserts an unrelated QR byte
array returns `kind === "ignored"`.

- [ ] **Step 6: Commit**

```bash
git add crates/optical-wasm web/receiver/tests
git commit -m "Run the same verified decoder on every phone browser" \
  -m "Expose the Rust receive state machine through a narrow serialized WASM boundary." \
  -m "Constraint: Browser input remains untrusted binary data" \
  -m "Rejected: Duplicating decoder state in JavaScript | it would create a second protocol implementation" \
  -m "Confidence: high" \
  -m "Scope-risk: moderate" \
  -m "Tested: native parity test, wasm-pack build, browser module smoke test"
```

---

### Task 6: Native Desktop Sender

**Files:**

- Create: `apps/desktop-sender/Cargo.toml`
- Create: `apps/desktop-sender/src/main.rs`
- Create: `apps/desktop-sender/src/app.rs`
- Create: `apps/desktop-sender/src/transfer.rs`
- Create: `apps/desktop-sender/src/qr.rs`
- Create: `apps/desktop-sender/tests/frame_pipeline.rs`
- Modify: `Cargo.toml`

**Interfaces:**

- Consumes: files or text selected in the native UI and `optical-core` packed
  frames.
- Produces: continuous QR module images and visible transfer status.

- [ ] **Step 1: Write a failing frame-pipeline test**

Create a test that packs `"hello from desktop"`, builds `Transfer::new`, asks
for sequences 0 and 1, and asserts each returned `QrFrame` has a square module
matrix, a four-module quiet zone, and payload bytes whose parsed headers have
the expected sequence and common stream identity.

- [ ] **Step 2: Add the desktop manifest**

```toml
[package]
name = "desktop-sender"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
eframe = "0.36.1"
mime_guess = "2.0.5"
optical-core = { path = "../../crates/optical-core" }
qrcodegen = "1.8.0"
rand = "0.10.2"
rfd = "0.17.2"
```

Add `"apps/desktop-sender"` to the root workspace `members` array before
running Cargo.

Run `cargo test -p desktop-sender`; expect failure because `Transfer` and
`QrFrame` are missing.

- [ ] **Step 3: Implement transfer planning and QR generation**

Define:

```rust
pub struct TransferSettings {
    pub frame_bytes: usize,
    pub frames_per_second: u32,
    pub ecc: Ecc,
    pub grid_codes: usize,
}

pub struct Transfer {
    encoder: LtEncoder,
    header: FrameHeader,
    next_seq: u32,
}

impl Transfer {
    pub fn from_file(name: &str, media_type: &str, bytes: &[u8], settings: TransferSettings)
        -> Result<Self, OpticalError>;
    pub fn from_text(text: &str, settings: TransferSettings) -> Result<Self, OpticalError>;
    pub fn next_payload(&mut self) -> Vec<u8>;
}
```

Port upstream frame-capacity calculations so requested bytes map to a valid QR
version and leave exactly 22 bytes for the header. Generate binary-mode QR with
explicit ECC and fixed mask. Convert modules into an `egui::ColorImage` with a
four-module quiet zone and nearest-neighbor scaling only.

- [ ] **Step 4: Implement the desktop UI and timing loop**

`SenderApp` owns idle/file/text/streaming state. Use `rfd::FileDialog` for
selection. Read the selected file once, reject size before compression, infer
media type with `mime_guess`, and construct a transfer on the UI thread with a
visible preparation state. Precompute at least three frames, display frames
against an absolute monotonic deadline, and call
`ctx.request_repaint_after(next_deadline - now)`.

The screen includes:

- file/text tabs;
- selected filename and original/transmitted sizes;
- reliability-first defaults of 1465 frame bytes, 20 FPS, ECC L, one code;
- start/stop controls;
- QR stage with fullscreen toggle;
- sequence, cycle, payload, frame-rate, and compression status;
- persistent warning: `Not encrypted: any camera can read this transfer.`

- [ ] **Step 5: Verify native behavior**

Run:

```bash
cargo fmt --all -- --check
cargo test -p desktop-sender
cargo clippy -p desktop-sender --all-targets -- -D warnings
cargo build -p desktop-sender --release
```

Manually open the release binary, select a small Unicode-named file, start and
stop twice, switch to text, and verify frame sequence advances without UI
freezes or stale frames.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop-sender crates/optical-core
git commit -m "Turn desktop files into a reliable optical stream" \
  -m "Add the cross-platform native sender with deterministic QR framing and reliability-first defaults." \
  -m "Constraint: One code at 1465 bytes and 20 FPS is the default" \
  -m "Rejected: A desktop webview | native egui keeps the sender self-contained" \
  -m "Confidence: medium" \
  -m "Scope-risk: broad" \
  -m "Directive: Measure camera success before raising default density" \
  -m "Tested: frame pipeline, cargo test, clippy, native manual smoke"
```

---

### Task 7: Offline Phone Receiver PWA

**Files:**

- Create: `web/receiver/index.html`
- Create: `web/receiver/styles.css`
- Create: `web/receiver/app.js`
- Create: `web/receiver/worker.js`
- Create: `web/receiver/manifest.webmanifest`
- Create: `web/receiver/service-worker.js`
- Create: `web/receiver/icons/icon.svg`
- Copy: `vendor/decimen-codec/decimen_codec.js`
- Copy: `vendor/decimen-codec/decimen_codec.wasm`
- Copy: `vendor/decimen-codec/LICENSE.zxing-cpp`
- Copy: `vendor/decimen-codec/NOTICE.md`
- Create: `scripts/build-web.sh`
- Create: `scripts/check-vendor.py`
- Create: `scripts/serve-web.py`

**Interfaces:**

- Consumes: camera frames, Decimen codec QR results, and `WasmReceiver` events.
- Produces: live acquisition/progress UI and verified text/file results.

- [ ] **Step 1: Write static receiver contract checks**

Create `scripts/check-web.py` using only Python's standard library. It parses
the service-worker asset list and asserts every listed file exists, the HTML
contains camera/start/result regions, all script/style URLs are relative to the
receiver root, and no `http://` or `https://` runtime asset appears in HTML,
CSS, JavaScript, or the manifest.

Run:

```bash
python3 scripts/check-web.py
```

Expected: failure because receiver assets do not exist.

- [ ] **Step 2: Vendor the pinned QR decoder and build WASM**

Copy `decimen_codec.js`, `decimen_codec.wasm`, and their notices from upstream
commit `5301fc2020716b5a97d4edf6e1793c3dedbf6e17`. Record SHA-256 values in
`vendor/decimen-codec/SHA256SUMS`. Run:

```bash
wasm-pack build crates/optical-wasm --target web --out-dir ../../web/receiver/pkg
python3 scripts/check-vendor.py
```

Expected: generated Rust WASM package and matching decoder checksums.

Create `scripts/build-web.sh` as the single web build entry point:

```sh
#!/bin/sh
set -eu
wasm-pack build crates/optical-wasm --target web --out-dir ../../web/receiver/pkg
mkdir -p web/receiver/vendor/decimen-codec
cp vendor/decimen-codec/decimen_codec.js web/receiver/vendor/decimen-codec/
cp vendor/decimen-codec/decimen_codec.wasm web/receiver/vendor/decimen-codec/
python3 scripts/check-vendor.py
python3 scripts/check-web.py
```

- [ ] **Step 3: Build the camera worker**

`worker.js` imports the pinned decoder, initializes it once, copies RGBA pixels
into WASM memory, calls `readTracked` for known crops, falls back to `readFull`,
and returns transferable binary payloads plus region boxes. Every allocated
pointer and embind vector is released in `finally`. A warm-up message performs
an 8-by-8 blank decode before reporting readiness.

Message contract:

```javascript
// input
{ id, bitmap, width, height, offsetX, offsetY, full, quad, modules }

// output
{ id, symbols: [{ bytes, box, quad, modules, tracked }], sightings }
```

- [ ] **Step 4: Build the receiver application**

`app.js` imports `WasmReceiver`, creates a worker pool sized from hardware
concurrency with an upper bound of four, and implements:

```javascript
async function startCamera() { /* rear camera, exact FPS then ideal fallback */ }
function captureFrame(now, metadata) { /* tracked crops plus periodic full scan */ }
function handleSymbols(symbols) { /* receiver.add_frame(bytes), render event */ }
function stopCamera() { /* generation bump, track stop, workers remain warm */ }
function exposeResult(event) { /* textContent or Blob URL after Complete */ }
```

Use `textContent` for snippets. Use a Blob URL and sanitized `download` value
for files. Revoke any previous Blob URL before replacement and on page unload.
Progress shows unique frames, solved blocks, percentage estimate, elapsed time,
and active tracked regions. Busy workers cause frame drops rather than queues.

- [ ] **Step 5: Build accessible offline UI and service worker**

Create a high-contrast responsive camera UI with start/stop, rear-camera
selector, viewfinder overlay, progress, result sheet, save/copy actions, and
plain-language troubleshooting. Every control has a label, keyboard focus is
visible, status changes use a polite live region, and reduced-motion preference
disables decorative movement.

The service worker precaches only versioned local assets: HTML, CSS, app and
worker scripts, both WASM modules, generated bindings, manifest, and icon. It
uses cache-first for these immutable assets and does not cache recovered files
or camera data.

- [ ] **Step 6: Verify offline static bundle**

Run:

```bash
python3 scripts/check-web.py
mkdir -p .tooling
openssl req -x509 -newkey rsa:2048 -nodes -days 7 \
  -keyout .tooling/dev-key.pem -out .tooling/dev-cert.pem \
  -subj "/CN=localhost"
python3 scripts/serve-web.py --directory web/receiver --port 8443 \
  --cert .tooling/dev-cert.pem --key .tooling/dev-key.pem
```

In a phone or browser device session, grant camera access, confirm worker/WASM
readiness, install the PWA, disable network access, reload, and confirm the
receiver still reaches the camera-ready screen.

- [ ] **Step 7: Commit**

```bash
git add web/receiver vendor/decimen-codec scripts
git commit -m "Let a phone receive without installing a native application" \
  -m "Combine browser camera control, the pinned QR decoder, and the Rust WASM core in an offline PWA." \
  -m "Constraint: Camera access requires a secure context during installation" \
  -m "Rejected: Native iOS and Android shells | the PWA covers the first release" \
  -m "Confidence: medium" \
  -m "Scope-risk: broad" \
  -m "Directive: Keep every runtime asset local and in the service-worker contract check" \
  -m "Tested: static asset contract, decoder checksums, offline reload, camera smoke"
```

---

### Task 8: End-to-End Transfer and Portable Releases

**Files:**

- Create: `tests/end-to-end/README.md`
- Create: `scripts/package_release.py`
- Create: `scripts/check-release.py`
- Create: `.github/workflows/ci.yml`
- Create: `.github/workflows/release.yml`
- Modify: `README.md`

**Interfaces:**

- Consumes: complete sender binary, receiver static bundle, and Cargo tests.
- Produces: verified platform archives, receiver archive, and checksums.

- [ ] **Step 1: Add a failing release-contract check**

Extend `scripts/check-web.py` or add `scripts/check-release.py` to assert an
assembled release directory contains:

```text
LICENSE
NOTICE.md
SOURCE.txt
SHA256SUMS
desktop-sender executable or macOS app bundle
receiver/index.html
receiver/service-worker.js
receiver/pkg/optical_wasm_bg.wasm
receiver/vendor/decimen_codec.wasm
```

Run it against an empty `dist/`; expect failure listing missing artifacts.

- [ ] **Step 2: Create CI verification matrix**

`ci.yml` runs on Linux, Windows, macOS Intel, and macOS arm64 where hosted
runners permit. Every job checks out submodules/assets, installs Rust 1.98.1,
runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test
--workspace`. A Linux web job installs `wasm-pack`, builds `optical-wasm`,
verifies vendored decoder hashes, and runs `scripts/check-web.py`.

- [ ] **Step 3: Package release artifacts**

`package_release.py` takes an explicit target triple and executable path,
creates a target-specific staging directory, copies legal/source files and the
receiver bundle, writes `SOURCE.txt` with repository and commit, creates a zip
archive with Python's `zipfile`, and writes SHA-256 checksums with `hashlib`.
It exits if the tree is dirty or any required file is missing.

`release.yml` triggers on `v*` tags, builds each native target, combines macOS
architectures into a universal app when both artifacts are available, builds
the receiver once, invokes the packaging script, and attaches archives plus a
top-level `SHA256SUMS` to the release.

- [ ] **Step 4: Run automated verification**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
wasm-pack build crates/optical-wasm --target web --out-dir ../../web/receiver/pkg
python3 scripts/check-vendor.py
python3 scripts/check-web.py
python3 scripts/package_release.py --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --binary target/release/desktop-sender
python3 scripts/check-release.py dist
```

Expected: all checks pass and the archive checksum verifies.

- [ ] **Step 5: Run the hardware acceptance matrix**

Record results in `tests/end-to-end/README.md` with date, sender OS, phone and
browser, payload, settings, elapsed time, and SHA-256 result. Required cases:

- macOS arm64 sender to iPhone Safari: Unicode text and 1 MiB binary file;
- macOS arm64 sender to Android Chrome: Unicode filename and 1 MiB binary file;
- Windows or Linux sender to either phone: 1 MiB binary file;
- receiver opened after the sender has already advanced at least one full
  carousel;
- reduced-density fallback after deliberately failing a high-density stream;
- installed receiver with network disabled.

Every recovered file must match `sha256sum`/`shasum -a 256` on the sender.

- [ ] **Step 6: Update user documentation**

Document desktop download/start steps, HTTPS hosting and PWA installation,
offline use, reliable aiming distance, settings fallback order, 64 MiB limit,
supported platforms, checksum behavior, security warning, source availability,
and AGPL attribution. Include no network-transfer claims beyond the optical
channel itself.

- [ ] **Step 7: Commit**

```bash
git add .github scripts tests README.md
git commit -m "Make portable releases prove they can move real data" \
  -m "Add cross-platform builds, self-contained receiver packaging, integrity checks, and a hardware acceptance record." \
  -m "Constraint: Releases carry corresponding source and AGPL notices" \
  -m "Rejected: Platform installers in v0.1 | archives keep the first release reproducible" \
  -m "Confidence: medium" \
  -m "Scope-risk: broad" \
  -m "Directive: Do not publish a target without its release-contract and hardware evidence" \
  -m "Tested: workspace checks, web contract, packaging contract, hardware matrix"
```

---

## Final Verification

- [ ] Run `cargo fmt --all -- --check` with no changes.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings` with no warnings.
- [ ] Run `cargo test --workspace` with all tests passing.
- [ ] Build the WASM receiver and pass `scripts/check-web.py`.
- [ ] Verify vendored decoder checksums.
- [ ] Complete and record every hardware acceptance case.
- [ ] Build one release archive and pass `scripts/check-release.py`.
- [ ] Confirm `git status --short` is empty.
- [ ] Confirm distributed artifacts contain `LICENSE`, `NOTICE.md`, and source information.
