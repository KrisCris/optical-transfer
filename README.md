# Optical Transfer

Move a file or a piece of text from a computer to a phone with nothing between
them but a camera and a screen. No network, no pairing, no account. The sender
plays the file as an endless stream of animated QR codes; the phone points at
it and rebuilds the file.

A Rust port of [Decimen Optical Transfer](https://github.com/bashalarmistalt/decimen-optical-transfer)
v0.5.3, byte-compatible with it on the wire.

## Security

**This transfer is not encrypted.** Any camera pointed at the sending screen
can read it, and the receiver's UI says so continuously. The only property on
offer is that no network sits between the two devices.

Nothing is shown to the user until two independent checks agree: the frame
header's FNV-1a over the assembled container, and the container's own SHA-256
over the file inside it. Received text is rendered as text and never as HTML.
Filenames are reduced to a basename on both ends.

## Layout

| Path | What it is |
| --- | --- |
| `crates/optical-core` | The protocol: wire-v3 framing, the systematic-carousel fountain codec, DCF2 containers, frame capacity. Platform-neutral, no I/O. |
| `crates/optical-wasm` | The receive-side state machine plus a thin `wasm-bindgen` wrapper. |
| `apps/desktop-sender` | The native sender, an `eframe` window. |
| `web/receiver` | The offline receiver PWA. |
| `vendor/decimen-codec` | The pinned zxing-cpp QR decoder build, checksummed. |
| `scripts/upstream-conformance` | Cross-implementation checks against the upstream baseline. |

Both ends speak only the shared wire format. There is no handshake and no back
channel: a receiver locking onto a stream halfway through rebuilds the file
from wherever it started.

## Build

Prerequisites: Rust 1.98.1 (`rust-toolchain.toml` pins it and the
`wasm32-unknown-unknown` target), `wasm-pack` 0.15.0, and Python 3 for the
check scripts. On Linux the sender also needs the GTK, X11/Wayland and GL
development headers — see `.github/workflows/ci.yml` for the exact package
list.

```sh
cargo test --workspace          # the protocol and both ends
./scripts/build-web.sh          # the receiver bundle, vendor checks included
cargo run -p desktop-sender --release
```

## Using it

**Sender.** Choose a file or paste text, press *Start sending*, and press
*Fullscreen*. The defaults — 1465 bytes per frame at 24 fps, ECC L, one code —
are the conservative pairing upstream falls back to when a transfer is
struggling, chosen because this build has no field measurements of its own yet.
Raise them once you know what your screen and phone can do.

**Receiver.** Serve `web/receiver/` over HTTPS and open it on the phone.
Browsers refuse camera access outside a secure context, and `localhost` is not
where the phone is:

```sh
mkdir -p .tooling
openssl req -x509 -newkey rsa:2048 -nodes -days 7 \
  -keyout .tooling/dev-key.pem -out .tooling/dev-cert.pem -subj "/CN=localhost"
python3 scripts/serve-web.py --directory web/receiver --port 8443 \
  --cert .tooling/dev-cert.pem --key .tooling/dev-key.pem
```

Install it to the home screen and it works with no network at all afterwards;
every runtime asset is precached, and `scripts/check-web.py` fails the build if
anything would reach out at runtime.

**If it is not picking anything up.** Fill about half the camera view with the
code, hold still, turn the sending screen's brightness up, and lower bytes per
frame and frames per second. Fewer, larger, slower codes beat dense fast ones.

## Limits

- Files up to 64 MiB; text snippets up to 4 MiB.
- A frame numbers its source blocks in 16 bits, so small frame sizes cap out
  well below the file limit — about 30 MB at 500 bytes per frame. The sender
  refuses before starting and names a setting from its own list that works.
- The first release targets Windows x86-64, Linux x86-64, macOS x86-64, macOS
  arm64, and modern iOS and Android browsers.

## What is verified

The wire format is pinned against upstream's own golden vectors, and
`scripts/upstream-conformance/` checks both directions against the upstream
implementation *running* — including that `flate2` and the browser's
`CompressionStream` can read each other's gzip, which no amount of reading the
source can settle. `web/receiver/tests/optical-loop.html` takes the sender's
QR modules through the shipped zxing-cpp decoder into the Rust receiver.

**No transfer has yet been carried by a real camera.** See
`tests/end-to-end/README.md` for what that leaves uncovered.

## License and attribution

AGPL-3.0-or-later, as a derivative of Decimen Optical Transfer v0.5.3
(commit `5301fc2020716b5a97d4edf6e1793c3dedbf6e17`). See `LICENSE` and
`NOTICE.md` for the full attribution and the zxing-cpp Apache-2.0 notice.
Distributed archives carry both, plus a `SOURCE.txt` naming the commit they
were built from.
