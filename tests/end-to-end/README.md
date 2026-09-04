# Hardware acceptance

## Status: not yet run

**No transfer in this repository has been carried by an actual camera.** Every
case below is unrecorded. This file is the form, not the result, and it says so
because a checklist of unticked boxes that reads like evidence is worse than no
checklist at all.

What *has* been verified, and what it does and does not cover:

| Evidence | Covers | Does not cover |
| --- | --- | --- |
| `cargo test --workspace` (50 tests) | The wire format against upstream's own golden vectors; sender to receiver in bytes | Anything optical |
| `scripts/upstream-conformance/` | Bidirectional interop with the pinned upstream v0.5.3, gzip included | Anything optical |
| `web/receiver/tests/optical-loop.html` | Sender-drawn QR modules → the shipped zxing-cpp build → the Rust receiver, over a lossy stream | A lens, exposure, focus, glare, motion blur, rolling shutter, screen refresh, or ambient light |
| `web/receiver/tests/decoder.html` | The compiled WASM receiver in a real browser | The camera path |

The optical-loop test is the closest thing to proof available without hardware:
it reads the sender's real pixels with the receiver's real decoder. What it
cannot tell you is whether a phone held at arm's length in a lit room can
resolve those modules at all — which is precisely what the defaults are tuned
for and precisely what nobody has measured yet.

## How to run a case

1. `./scripts/build-web.sh`
2. Serve the receiver over HTTPS so the phone will grant camera access.
   `getUserMedia` is refused on plain HTTP from anything but localhost, and
   the failure looks like a broken receiver rather than a browser policy:

   ```sh
   mkdir -p .tooling
   openssl req -x509 -newkey rsa:2048 -nodes -days 7 \
     -keyout .tooling/dev-key.pem -out .tooling/dev-cert.pem -subj "/CN=localhost"
   python3 scripts/serve-web.py --directory web/receiver --port 8443 \
     --cert .tooling/dev-cert.pem --key .tooling/dev-key.pem
   ```

3. `cargo run -p desktop-sender --release`, choose the payload, press
   **Start sending**, and use **Fullscreen**.
4. Open the served URL on the phone, accept the self-signed certificate, and
   press **Start camera**.
5. Compare the recovered file against the sender's own hash:
   `shasum -a 256 <file>` (macOS) or `sha256sum <file>` (Linux/Windows).

Record the result in the table below. A failure is a result: write down what
the screen said and what the settings were, because "it did not work" without
the frame size and the distance is not information anyone can act on.

## Required cases

| # | Sender | Receiver | Payload | Settings | Result |
| --- | --- | --- | --- | --- | --- |
| 1 | macOS arm64 | iPhone Safari | Unicode text | defaults | not run |
| 2 | macOS arm64 | iPhone Safari | 1 MiB binary | defaults | not run |
| 3 | macOS arm64 | Android Chrome | Unicode **filename**, 1 MiB binary | defaults | not run |
| 4 | Windows or Linux x86-64 | either phone | 1 MiB binary | defaults | not run |
| 5 | any | any | any | receiver started only *after* the sender has played a full carousel | not run |
| 6 | any | any | 1 MiB binary | deliberately fail at 2953 bytes / 60 fps, then fall back to the suggested lower setting | not run |
| 7 | any | any | any | receiver installed to the home screen, device in airplane mode, app reopened | not run |

Case 5 has a software analogue that passes
(`a_receiver_joining_mid_carousel_still_completes`); the hardware version also
exercises reacquisition, which the software one does not.

Case 7 is the only check of the service worker. It is currently unverified in
any form — service workers are disabled in the browser available during
development, confirmed by a one-line service worker failing to register
identically to the real one.

## Recording template

```
date:      2026-__-__
sender:    <os, arch, display refresh rate>
receiver:  <phone model, os version, browser>
payload:   <name, size>
settings:  <bytes/frame, fps, ecc, codes>
distance:  <roughly, cm>
elapsed:   <seconds from start of camera to result>
sha256:    <sender>  /  <receiver>   → match / MISMATCH
notes:     <anything surprising>
```
