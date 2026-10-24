# Optical Transfer

Move photos, files, or text between devices with nothing between them but a
camera and a screen. No network, no pairing, no account. The sender plays the
payload as an endless stream of animated QR codes; the receiving device points
its camera at it and rebuilds the file.

A Rust port of [Decimen Optical Transfer](https://github.com/bashalarmistalt/decimen-optical-transfer)
v0.5.3, byte-compatible with it on the wire: either sender works with either
receiver.

## Security

**This transfer is not encrypted.** Any camera pointed at the sending screen
can read it, and both ends say so continuously. The only property on offer is
that no network sits between the two devices.

Nothing is shown to the user until two independent checks agree: the frame
header's FNV-1a over the assembled container, and the container's own SHA-256
over the file inside it. Received text is rendered as text and never as HTML.
Top-level filenames are reduced to a basename on both ends. Folder paths inside
generated ZIPs are validated and sanitized before sending.

## Layout

| Path | What it is |
| --- | --- |
| `crates/optical-core` | Protocol, fountain code, containers, ZIP bundling, transfer and QR renderer. Platform-neutral, no I/O. |
| `crates/optical-wasm` | Sender and receiver state plus `wasm-bindgen` wrappers. |
| `apps/desktop-sender` | The native sender, an `eframe` window. |
| `web/app` | The offline Send/Receive PWA. |
| `vendor/decimen-codec` | The pinned zxing-cpp QR decoder build, checksummed. |
| `scripts/upstream-conformance` | Cross-implementation checks against the upstream baseline. |

Both ends speak only the shared wire format. There is no handshake and no back
channel: a receiver locking onto a stream halfway through rebuilds the file
from wherever it started.

## Build

Prerequisites: Rust 1.98.1 (`rust-toolchain.toml` pins it and the
`wasm32-unknown-unknown` target), `wasm-pack` 0.15.0, Python 3 for the check
scripts, and Node.js 24 for the browser logic tests and the loopback
benchmark. On Linux the desktop sender also needs the GTK, X11/Wayland and GL
development headers; `.github/workflows/ci.yml` has the exact package list.

```sh
cargo test --workspace                 # protocol, fountain, both ends
node --test web/app/tests/*.test.mjs   # capture scheduler, telemetry, flip clock
./scripts/build-web.sh                 # wasm, vendor and static checks
cargo run -p desktop-sender --release  # or: … -- report.pdf photos/
```

## Using it

**Speed presets.** Both senders offer the same four, and *Fast* is the
default:

| Preset | Codes on screen | Frames/s | Bytes per code | On the wire |
| --- | --- | --- | --- | --- |
| Reliable | 1 | 24 | 1465 | 34 KiB/s |
| Balanced | 2 | 30 | 2331 | 135 KiB/s |
| Fast | 1 | 60 | 2953 | 172 KiB/s |
| Max | 4 | 60 | 2953 | 687 KiB/s |

*Fine-tune* sets each value directly, including the error-correction level.
Frame rate and code count change a running stream in place. Bytes per code and
ECC are baked into every frame, so changing them restarts the stream with a
new session. A payload smaller than one full code is sent at its own size, so
a short text rides a small, coarse code instead of a padded version 40.

**Desktop sender.** Drop files or folders on the window, choose them with the
picker, pass them on the command line, or switch to *Text*. Press *Start
sending*, then *Full screen* (or F). Escape or a click leaves full screen.
Several files or a folder travel as one `transfer.zip`, built by the same code
as the browser sender's. Hidden files and symbolic links inside a folder are
skipped.

**Web app.** Serve `web/app/` and open it on both devices. Browsers grant the
camera only to a secure page; `localhost` qualifies, but a phone needs HTTPS:

```sh
mkdir -p .tooling
openssl req -x509 -newkey rsa:2048 -nodes -days 7 \
  -keyout .tooling/dev-key.pem -out .tooling/dev-cert.pem -subj "/CN=localhost"
python3 scripts/serve-web.py --directory web/app --port 8443 \
  --cert .tooling/dev-cert.pem --key .tooling/dev-key.pem
```

Install it to the home screen and it works with no network at all afterwards.
Every runtime asset is precached, and `scripts/check-web.py` fails the build if
anything would reach out at runtime or if any module fails to parse.

*Send* takes files, a folder (iOS 18.4 or newer), or text, with the same
presets. Tap the code for full screen. The page holds a screen wake lock where
the browser allows one. There is no acknowledgement channel, so stop when the
receiver shows the file.

*Receive* opens the rear camera at 60 fps where the device allows it. On a
Mac, an iPhone on the same Apple Account shows up as a Continuity Camera, and
the receiver opens it automatically when no camera has been picked; the Mac's
cores then do the decoding. The first start on a Mac may still open the
built-in camera, because browsers hide camera names until permission is
granted. The settings popover picks the camera, capture width and decode
worker count, and the choice of camera is remembered.

**What the receiver shows.** The transfer card has recovered percent, current
rate, time remaining and elapsed time. Under them sit a one-minute chart of
recovered data rate and a map with one pixel per source block, lit as each
block is solved. Telemetry splits into the stream:

- payload and block dimensions
- unique, duplicate and redundant frames
- equations parked for the solver, and eliminations run
- catch rate (frames read over frames sent)
- overhead (frames used per source block)
- average and peak rate

and the pipeline:

- capture and decode rates
- codes read and codes locked
- scan mode and worker count
- tracked-decode hits
- per-path decode latency
- dropped captures
- the camera's actual resolution and frame rate

Elapsed time starts at the capture that produced the first accepted frame, so
aiming is not counted. Use an incompressible file when comparing speeds.

**What the sender shows.** The position in the carousel (systematic sweep or
repair frames), payload and compression, QR version, block count and size,
the raw rate and the best case for one clean sweep. It also shows frames
shown, the flip rate measured against its target, and the display's refresh
rate. A window that stalls (hidden, occluded or throttled) says so, because a
frozen stream otherwise looks like a receiver failure.

**If it is not picking anything up.** Fill about half the camera view with the
code, hold still, turn the sending screen's brightness up, and step down a
preset. Fewer, larger, slower codes beat dense fast ones.

## How it is fast

- **Sender display.** Every code is drawn at a whole number of device pixels
  per module and smooth-stretched for the remainder. With several codes on
  screen, cell *j* flips at phase *j/N* of the frame interval, so an exposure
  straddling a flip loses one code, not all of them. Every cell that has come
  due flips on the same display frame, so codes × fps is not capped by the
  refresh rate. The grid shape follows the screen's aspect. Encoding runs off
  the drawing thread: a Web Worker in the browser, a thread on the desktop.
- **Receiver capture.** One capture per camera frame (`requestVideoFrameCallback`).
  Each locked code gets its own padded crop on its own worker, using zxing's
  tracked fast path. Full-frame scans run every 100 ms while acquiring, 250 ms
  with a code missing and 1500 ms when locked, and a due scan takes one worker
  instead of the whole frame. A busy pool drops the frame instead of queueing
  a stale one. The main thread does no per-frame DOM work.
- **Decoder.** The LT peeling decoder finishes with Gaussian elimination once
  the parked equations reach full rank. At heavy loss that completes inside
  the first repair half of the carousel (under 1.2× overhead at 40–75% loss in
  the fountain tests) instead of waiting for peeling to get lucky.

`scripts/loopback-bench.mjs` measures the receive pipeline end to end in
headless Chrome. The browser sender's own display is re-filmed by a simulated
1280×960 camera with blur, tilt and contrast loss, and fed through the real
capture and decode path. There is no lens, glare or rolling shutter, so this
measures scheduling and decoding. Here is a 1 MiB incompressible file at 2953
bytes per code and 60 fps, compared against a replay of this repository's
previous receiver scheduler:

| Receiver | Codes | Time | Average | Catch rate | Overhead |
| --- | --- | --- | --- | --- | --- |
| previous | 1 | 14.1 s | 73 KiB/s | 0.89 | 1.30 |
| current | 1 | 6.4–7.9 s | 130–160 KiB/s | 0.89–0.99 | 1.06–1.18 |
| previous | 4 | 7.7 s | 133 KiB/s | 0.28 | 1.42 |
| current | 4 | 1.6–2.1 s | 494–644 KiB/s | 0.75–0.98 | 1.03–1.15 |

Ranges are across repeated runs on one machine; the ceiling at four codes is
the 687 KiB/s on the wire.

```sh
python3 scripts/serve-web.py --directory web/app --port 8765 &
node scripts/loopback-bench.mjs --runs "codes=4&fps=60" --repeat 3
```

## Limits

- Files up to 64 MiB; text snippets up to 4 MiB.
- Up to 256 files per selection. Multiple files share the 64 MiB limit,
  including ZIP headers. ZIP entries are stored uncompressed.
- A frame numbers its source blocks in 16 bits, so small frame sizes cap out
  well below the file limit — about 30 MB at 500 bytes per code. The sender
  refuses before starting and names a setting from its own list that works.
- The desktop app sends only; receiving is the web app's job.
- The first release targets Windows x86-64, Linux x86-64, macOS x86-64, macOS
  arm64, and modern iOS and Android browsers.

## What is verified

The wire format is pinned against upstream's own golden vectors, and
`scripts/upstream-conformance/` checks both directions against the upstream
implementation *running*. That includes checking that `flate2` and the
browser's `CompressionStream` can read each other's gzip. In the browser:

- `web/app/tests/optical-loop.html` takes sender-drawn QR modules through the
  shipped zxing-cpp build into the Rust receiver, over a lossy stream and for
  file, text and ZIP payloads.
- `decoder.html` exercises the compiled WASM receiver API.
- `loopback.html` runs the full camera pipeline.

**Text and a 1 MiB binary have been verified on an iPhone 14 Plus in Safari**
with the earlier receiver: 62.3 seconds at 16.4 KiB/s, with SHA-256
verification. The rebuilt receiver and multi-code senders have not yet been
measured on a real camera; `tests/end-to-end/README.md` lists the runs still
outstanding.

## License and attribution

AGPL-3.0-or-later, as a derivative of Decimen Optical Transfer v0.5.3
(commit `5301fc2020716b5a97d4edf6e1793c3dedbf6e17`). See `LICENSE` and
`NOTICE.md` for the full attribution and the zxing-cpp Apache-2.0 notice.
Distributed archives carry both, plus a `SOURCE.txt` naming the commit they
were built from.
