# Optical Transfer

Move photos, files, or text between devices with nothing between them but a
camera and a screen. No network, no pairing, no account. The sender plays the
payload as an endless stream of animated QR codes; the receiving device points
its camera at it and rebuilds the file.

A Rust port of [Decimen Optical Transfer](https://github.com/bashalarmistalt/decimen-optical-transfer)
v0.5.3. Its receivers read upstream's wire format (v3). Its senders default
to a faster format of their own, v4, which uses RaptorQ coding and can add
colour codes. Switched to v3, either sender works with upstream's receiver.
[docs/wire-v4.md](docs/wire-v4.md) specifies v4.

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
| `crates/optical-core` | Both wire formats, both fountain codes (LT and RaptorQ), containers, ZIP bundling, transfer and QR renderer. Platform-neutral, no I/O. |
| `crates/optical-wasm` | Sender and receiver state plus `wasm-bindgen` wrappers. |
| `apps/desktop-sender` | The native app, an `eframe` window. Sends everywhere; receives through a camera on macOS (the `receive` feature). |
| `web/app` | The offline Send/Receive PWA. |
| `vendor/decimen-codec` | The pinned zxing-cpp QR decoder build, checksummed. |
| `scripts/upstream-conformance` | Cross-implementation checks against the upstream baseline. |
| `docs` | Design notes: `wire-v4.md` covers the v4 format and colour codes. |

The two ends share nothing but the frames. There is no handshake and no back
channel: a receiver locking onto a stream halfway through rebuilds the file
from wherever it started, and tells the wire versions apart frame by frame.

## Build

Prerequisites: Rust 1.98.1 (`rust-toolchain.toml` pins it and the
`wasm32-unknown-unknown` target), `wasm-pack` 0.15.0, Python 3 for the check
scripts, and Node.js 24 for the browser logic tests and the loopback
benchmark. On Linux the desktop sender also needs the GTK, X11/Wayland and GL
development headers; `.github/workflows/ci.yml` has the exact package list.

```sh
cargo test --workspace                 # both wires, both fountains, both ends
node --test web/app/tests/*.test.mjs   # capture scheduler, telemetry, flip clock, colour unmixing
./scripts/build-web.sh                 # wasm, vendor and static checks
cargo run -p desktop-sender --release  # or: … -- report.pdf photos/
cargo run -p desktop-sender --release --features receive  # macOS, adds Receive
```

## Using it

**Speed presets.** Both senders offer the same five, and *Fast* is the
default:

| Preset | Codes on screen | Frames/s | Bytes per code | On the wire (v4) |
| --- | --- | --- | --- | --- |
| Reliable | 1 | 24 | 1465 | 34 KiB/s |
| Balanced | 2 | 30 | 2331 | 136 KiB/s |
| Fast | 1 | 60 | 2953 | 172 KiB/s |
| Colour | 1, ×3 layers | 60 | 2953 | 516 KiB/s |
| Max | 4 | 60 | 2953 | 688 KiB/s |

*Colour* puts three codes in one, in the red, green and blue channels. It only
pays while the code fills most of the camera's view, and has yet to meet a real
camera (see [docs/wire-v4.md](docs/wire-v4.md)).

*Fine-tune* sets each value directly. That includes the error-correction
level, 120 fps for a display that refreshes at least that fast, colour, and
the wire format: v4, or v3 for a receiver running upstream Decimen, which
reads neither v4 nor colour. A code never changes twice within one refresh, so
on a slower display the stream runs at the refresh rate and the sender's
Display line says so.
Frame rate and code count change a running stream in place. Bytes per code,
ECC, wire format and colour change what each frame looks like, so changing
any of them restarts the stream. A v4 receiver keeps what it already has
across a restart that leaves the bytes per code alone. A payload smaller than
one full code is sent at its own size, so a short text rides a small, coarse
code instead of a padded version 40.

**Desktop sender.** Drop files or folders on the window, choose them with the
picker, pass them on the command line, or switch to *Text*. Press *Start
sending*, then *Full screen* (or F). Escape or a click leaves full screen.
Several files or a folder travel as one `transfer.zip`, built by the same code
as the browser sender's. Hidden files and symbolic links inside a folder are
skipped.

**Desktop receiver (macOS).** Built with `--features receive`, the window gets
a *Send / Receive* switch. Receive opens a camera through AVFoundation and
prefers an iPhone or iPad used as a Continuity Camera, since a Mac's built-in
camera faces the user rather than the other screen. It asks for 1280×960 at
60 fps and takes the closest format the camera offers. Codes are decoded by
zxing-cpp on a pool of threads, scheduled the way the web receiver schedules
them: one padded crop per locked code, with a full-frame scan every 100, 250
or 1500 ms depending on how many codes are locked. Frames that arrive while
every thread is busy are dropped rather than queued. The panel shows the same
preview, progress, rate chart, block map and telemetry as the web receiver,
and a finished file is offered for saving. Run from `cargo run` the binary is
not an app bundle, so macOS attributes the camera to the terminal that started
it; the packaged `.app` asks for itself.

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

*Receive* opens the rear camera at 60 fps where the device allows it; the
settings popover can ask for 30 or 120 instead, and the telemetry's Camera
line shows the rate delivered and the camera's ceiling. On a
Mac, an iPhone on the same Apple Account shows up as a Continuity Camera, and
the receiver opens it automatically when no camera has been picked; the Mac's
cores then do the decoding. The first start on a Mac may still open the
built-in camera, because browsers hide camera names until permission is
granted. The settings popover picks the camera, capture width, capture rate
and decode worker count, and the choice of camera is remembered.

**What the receiver shows.** The transfer card has recovered percent, current
rate, time remaining and elapsed time. Under them sit a one-minute chart of
recovered data rate and a map with one pixel per source block (per source
symbol on v4), lit as each is recovered. Telemetry splits into the stream:

- payload and block dimensions
- the format: wire version, and colour while colour codes are arriving
- unique, duplicate and redundant frames
- on v3, equations parked for the solver and eliminations run; on v4, repair
  symbols held and decode attempts
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

**What the sender shows.** The wire format and the stream's id. Then where the
stream is: on v4 the systematic sweep and then a count of repair symbols,
which never repeat; on v3 the position in the carousel. Payload and
compression, QR version, source count and size, the raw rate and the best
case for one clean sweep. It also shows frames shown, the flip rate measured
against its target, and the display's refresh rate. A window that stalls (hidden, occluded or throttled) says so, because a
frozen stream otherwise looks like a receiver failure.

**If it is not picking anything up.** Fill about half the camera view with the
code, hold still, turn the sending screen's brightness up, and step down a
preset. Fewer, larger, slower codes beat dense fast ones. *Colour* needs the
code to fill most of the view; if the receiver's Format line never says
colour, use *Max* instead.

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
- **Coding.** Wire v4 codes with RaptorQ. A source block decodes from about
  as many symbols as it has, whichever ones arrived, so a receiver needs one
  frame per source symbol however many it missed. Repair symbols never repeat,
  so however long a receiver watches, every frame it catches is new. v3's LT
  decoder finishes with Gaussian elimination once the parked equations reach
  full rank, but in the benchmark below it needed up to 1.59 frames per block
  at 1 MiB and up to 1.99 at 16 MiB.
- **Colour.** A colour code carries three frames. The receiver unmixes the
  camera's channels blind, from the code's own colour statistics, and reads
  each channel as an ordinary QR code.

`scripts/loopback-bench.mjs` measures the receive pipeline end to end in
headless Chrome. The browser sender's own display is re-filmed by a simulated
1280×960 camera with blur, tilt and contrast loss, and fed through the real
capture and decode path. With `chroma=1` the camera also mixes its colour
channels, keeps colour at 4:2:0 and adds noise, which is what colour codes
have to survive. There is no lens, glare or rolling shutter, so this measures
coding, scheduling and decoding.

Here is an incompressible file at 2953 bytes per code and 60 fps. *Before*
is this repository just before v4 (wire v3 only), built separately and run on
the same page, interleaved with the current build. Times and rates are
medians, with the run-to-run range of frames needed per source block:

| File | Codes | Before (v3) | Now (v4) | Frames per block, v3 → v4 |
| --- | --- | --- | --- | --- |
| 1 MiB | 1 | 6.9 s, 148 KiB/s | 6.1 s, 168 KiB/s | 1.02–1.59 → 1.00 |
| 1 MiB | 1, colour | — | 2.1 s, 489 KiB/s | 1.00 |
| 1 MiB | 4 | 2.0 s, 518 KiB/s | 1.7 s, 601 KiB/s | 1.01–1.32 → 1.00 |
| 5 MiB | 1 | 44.2 s, 117 KiB/s | 33.1 s, 155 KiB/s | 1.30–1.41 → 1.00 |
| 16 MiB | 4 | 33.0 s, 497 KiB/s | 24.5 s, 668 KiB/s | 1.31–1.33 → 1.00 |

The wire carries 172 KiB/s at one code, 516 in colour and 688 at four. v4
runs within a few percent of that whenever the camera catches every frame,
97% of it at 16 MiB. v3 loses the difference to overhead, which grows with
the file and with loss. A transfer at four codes is mostly spent locking onto
the codes at 1 MiB, so the larger files show the difference better. These
are runs on an 8-core Apple Silicon Mac that other work was also using. The
1 and 5 MiB rows count runs whose page captured at 50 fps or more. The
16 MiB row is the two runs of each that caught at least 95% of frames. In the
rest, v3 needed up to 1.99 frames per block and v4 stayed at 1.00.
[docs/wire-v4.md](docs/wire-v4.md) has the run counts and ranges, every
larger run, and the checks that tell the wire's effect from the machine's.

```sh
python3 scripts/serve-web.py --directory web/app --port 8765 &
node scripts/loopback-bench.mjs --runs "codes=4&fps=60" "codes=4&fps=60&wire=3" --repeat 3
node scripts/loopback-bench.mjs --runs "codes=1&fps=60&color=1&chroma=1" --size 5242880
```

## Limits

- Files up to 128 MiB on wire v4 and 64 MiB on v3 (upstream's limit); text
  snippets up to 4 MiB. The v4 limit is a memory budget for the receiving
  browser tab, which holds about four times the file at the end. It has not
  been tried on a phone. Upstream's 64 MiB limits the packed container, so on
  v3 a file that does not compress and is within a few hundred bytes of
  64 MiB is refused, with a message suggesting v4.
- Up to 256 files per selection. Multiple files share the limit, including
  ZIP headers. ZIP entries are stored uncompressed.
- A v3 frame numbers its source blocks in 16 bits, so small frame sizes cap
  out well below the file limit, at about 30 MB at 500 bytes per code. The
  sender refuses before starting and names a setting from its own list that
  works. v4 has no such ceiling.
- The desktop app receives on macOS only. Windows and Linux builds send, and
  receiving there is the web app's job.
- The first release targets Windows x86-64, Linux x86-64, macOS x86-64, macOS
  arm64, and modern iOS and Android browsers.

## What is verified

Wire v3 is pinned against upstream's own golden vectors, and
`scripts/upstream-conformance/` checks both directions against the upstream
implementation *running*. That includes checking that `flate2` and the
browser's `CompressionStream` can read each other's gzip.
`crates/optical-core/tests/wire_v4.rs` pins v4. It covers the block layout,
decoding from about `K` symbols at 5–90% loss and from repair symbols alone,
interleaved blocks, refusing hostile headers, a layout for the largest
container a permitted file packs into, and receivers following a sender that
switches wire. Every offered frame size and ECC level, and a receiver
joining mid-stream, are run end to end over both wires.
Colour codes are checked end to end through two synthetic cameras with 4:2:0
chroma and crosstalk: the desktop receiver's tests and the loopback
benchmark. The web worker's unmixing has its own tests in
`web/app/tests/unmix.test.mjs`.
In the browser:

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
