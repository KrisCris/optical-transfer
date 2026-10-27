# Hardware acceptance

## Status: rebuilt receiver and senders awaiting their first camera runs

Everything below the next paragraph was measured with the *earlier* receiver
and single-code senders. Since then:

- the receiver's capture scheduler has been rebuilt (one crop per locked code,
  every free worker used, camera-frame callbacks);
- the decoder finishes with Gaussian elimination;
- both senders show up to nine codes at a time, with staggered flips;
- the default preset is *Fast*: 2953 bytes and one code, at most 36 fps,
  each code up a whole number of screen refreshes (about 28 ms);
- both senders send wire v4 (RaptorQ, `docs/wire-v4.md`) by default, and
  every receiver reads v4 and upstream's v3;
- a *Colour* preset stacks three codes in the red, green and blue channels.

In the headless loopback benchmark (`scripts/loopback-bench.mjs`) the new
receiver is 2.2× faster at one code and about 3× faster at four. v4 brings
the frames needed per source block down to 1.00, from up to 1.59 at 1 MiB and
up to 1.99 at 16 MiB on v3. Colour triples what one code carries. None of that says anything about a lens, so
cases 9–16 below are the next runs to make, and the earlier numbers are the
baseline to beat.

## Earlier results: text and first 1 MiB camera transfer verified

On 2026-09-06, a Mac-to-iPhone 14 Plus transfer of `Hello from my Mac`
succeeded in Safari. The user's receiver screenshot shows `snippet.txt`,
17 B, 0.8 seconds on the original camera-start timer, and SHA-256 verified,
with the exact sent text displayed.
This establishes a real-camera text transfer.

On 2026-09-07, baseline run 1 transferred the incompressible
`optical-test-1MiB.bin` (1,048,576 bytes) to the same iPhone 14 Plus. The user's
receiver screenshot reports **62.3 s**, **16.4 KiB/s**, and SHA-256 verified.
This uses the new first-accepted-capture-to-verification timer. Two further
runs with the same settings and positioning are pending; no median or frame
loss rate can be established from this single result.

The user's OG Decimen website comparison on 2026-09-07 received the same named
1 MiB file in **42.7 s**, displaying **24.0 KB/s** and SHA-256 verified. Its
summary showed capture FPS 60, decode FPS 36.0, recent goodput 24.7 KB/s,
847 new / 385 duplicate frames, K=727, and block length 1,443 B. Units and
labels here are reproduced as displayed. The block size matches our instructed
setting; sender FPS, positioning, and timing semantics were not independently
matched, so this is an observed comparison rather than an isolated diagnosis.

On 2026-09-08, the user reported a closer comparison: the same OG-generated QR
stream, with each receiver aimed before transmission, reached approximately
30 KB/s in OG and 15 KB/s here. Requested camera profiles differed: ours used
1920×1080 without a frame-rate constraint; upstream uses 1280×960 with an exact
60 fps request and an ideal-rate fallback. The receiver now matches that
profile. Whether it closes the hardware throughput gap remains unverified;
compare the actual Camera field and final file-throughput result on the next run.

The instructed settings were 1465 bytes/frame, 24 fps, ECC L, and one code;
the screenshot does not independently confirm sender settings. iOS version,
display refresh rate, distance, and an independently compared saved-file hash
were not recorded.

What *has* been verified, and what it does and does not cover:

| Evidence | Covers | Does not cover |
| --- | --- | --- |
| `cargo test --workspace` | Wire compatibility, shared sender/receiver, and file bundle checks | Anything optical |
| `node --test web/app/tests/*.test.mjs` | Capture scheduling (scan cadence, crops, region lifetime, worker pool), telemetry arithmetic, the flip clock and grid choice, formatting | Camera hardware or decode speed |
| `scripts/loopback-bench.mjs` | The real capture and decode pipeline, driven by the real sender display re-filmed at 1280×960 with blur, tilt and contrast loss | A lens, exposure, glare, rolling shutter, or a phone's CPU |
| `scripts/upstream-conformance/` | Bidirectional interop with the pinned upstream v0.5.3, gzip included | Anything optical |
| `web/app/tests/optical-loop.html` | Sender-drawn QR modules → the shipped zxing-cpp build → the Rust receiver, over a lossy stream | A lens, exposure, focus, glare, motion blur, rolling shutter, screen refresh, or ambient light |
| `web/app/tests/decoder.html` | The compiled WASM receiver in a real browser | The camera path |
| User-supplied iPhone 14 Plus screenshot, 2026-09-06 | Real-camera receipt of the exact 17-byte text, with receiver SHA-256 verification | Sustained throughput, large files, Unicode, or offline installation |
| User-supplied iPhone 14 Plus screenshot, 2026-09-07, baseline run 1 | 1 MiB binary received and SHA-256 verified in 62.3 s at 16.4 KiB/s | Repeatability, sender settings visible in evidence, decode/drop statistics, or an independent saved-file hash comparison |
| Browser web-sender UI run | Mixed photo/text/binary selection → Rust/WASM QR canvas → zxing → Rust receiver → ZIP; all three extracted files byte-identical in Python | Physical phone photo picker, a lens, or sustained hardware throughput |
| Browser folder-picker run | Selected `Trip` folder → worker/WASM → displayed QR → ZIP; `day-1/notes.txt` and `day-2/notes.txt` retain paths and exact bytes after extraction | Physical iPhone folder picker or empty directories |

The optical-loop test is the closest thing to proof available without hardware:
it reads the sender's real pixels with the receiver's real decoder. What it
cannot tell you is whether a phone held at arm's length in a lit room can
resolve those modules reliably across conditions. The text smoke test above
establishes one successful real-camera run; broader hardware validation remains.

## How to run a case

1. `./scripts/build-web.sh`
2. Serve the receiver over HTTPS so the phone will grant camera access.
   `getUserMedia` is refused on plain HTTP from anything but localhost, and
   the failure looks like a broken receiver rather than a browser policy:

   ```sh
   mkdir -p .tooling
   openssl req -x509 -newkey rsa:2048 -nodes -days 7 \
     -keyout .tooling/dev-key.pem -out .tooling/dev-cert.pem -subj "/CN=localhost"
   python3 scripts/serve-web.py --directory web/app --port 8443 \
     --cert .tooling/dev-cert.pem --key .tooling/dev-key.pem
   ```

3. `cargo run -p desktop-sender --release`, choose the payload, pick a
   preset, press **Start sending**, and use **Full screen** (or F).
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
| 2 | macOS arm64 | iPhone 14 Plus Safari | 1 MiB binary | defaults instructed; not visible in screenshot | run 1: received, SHA-256 verified, 62.3 s, 16.4 KiB/s; repeat runs and independent saved-file hash comparison pending |
| 3 | macOS arm64 | Android Chrome | Unicode **filename**, 1 MiB binary | defaults | not run |
| 4 | Windows or Linux x86-64 | either phone | 1 MiB binary | defaults | not run |
| 5 | any | any | any | receiver started only *after* the sender has played a full carousel | not run |
| 6 | any | any | 1 MiB binary | deliberately fail at 2953 bytes / 60 fps, then fall back to the suggested lower setting | not run |
| 7 | any | any | any | receiver installed to the home screen, device in airplane mode, app reopened | not run |
| 8 | iPhone Safari web sender | another device's browser | multiple photos plus a file | defaults; save ZIP and compare extracted contents | not run |
| 9 | macOS arm64 desktop | iPhone 14 Plus Safari | the same 1 MiB binary as case 2 | Fast (2953 B, 36 fps, 1 code) | not run |
| 10 | macOS arm64 desktop | iPhone 14 Plus Safari | the same 1 MiB binary | Balanced and Max (2 and 4 codes) | not run |
| 11 | web sender on a second screen | macOS desktop **Receive** mode, iPhone as Continuity Camera | 1 MiB binary | Fast, then Max | not run; see below |
| 12 | macOS arm64 desktop | iPhone 14 Plus Safari | the same 1 MiB binary | Fast on v4, then Fast with *Wire format* set to v3, three runs each, alternating | not run |
| 13 | macOS arm64 desktop | iPhone 14 Plus Safari | the same 1 MiB binary | Colour, with the code filling at least 80% of the camera view; capture rate 60, then 120 | not run |
| 14 | macOS arm64 desktop | iPhone 14 Plus Safari | the same 1 MiB binary | Max on v4 | not run |
| 15 | macOS arm64 desktop | iPhone 14 Plus Safari | 5 MiB and 16 MiB incompressible binaries | Max on v4, then Max on v3 | not run |
| 16 | macOS arm64 desktop | iPhone 14 Plus Safari | 64 MiB, then 128 MiB, incompressible | Max on v4 | not run |

Case 5 has a software analogue that passes
(`a_receiver_joining_mid_carousel_still_completes`); the hardware version also
exercises reacquisition, which the software one does not.

Case 7 remains unverified on phone hardware. Desktop browser automation has
verified an offline reload and a subsequent web-sender text transfer; that
does not establish iPhone home-screen installation or airplane-mode behavior.

## Case 11: the desktop receiver through Continuity Camera

The native Receive mode, filming another screen through an iPhone that macOS
offers as a camera. It has its own procedure because the receiver is the Mac:

1. Package the app so the camera prompt names Optical Transfer rather than a
   terminal, and so the Continuity Camera device type is enabled:

   ```sh
   ./scripts/build-web.sh
   cargo build -p desktop-sender --release --features receive
   python3 scripts/package_release.py --target aarch64-apple-darwin \
     --binary target/release/desktop-sender
   open "dist/aarch64-apple-darwin/Optical Transfer.app"
   ```

2. Mount the iPhone (locked, on a stand, same Apple Account, Wi-Fi and
   Bluetooth on) facing the second screen. It should appear in the *Camera*
   list as `… · Continuity`, and *Automatic* should name it.
3. On the second screen, open the web app's *Send* page with the same
   `optical-test-1MiB.bin` as case 2, choose *Fast*, start, and go full screen.
4. In the desktop app, switch to *Receive* and press *Start receiving*. Allow
   camera access the first time. Frame the codes so they fill most of the
   preview; each code that decodes gets a coloured bracket, and the HUD shows
   *locked* once every code is tracked.
5. Save the file when it verifies, compare hashes, and copy *Elapsed*, the
   *Avg / peak* rate and the rest of the Telemetry panel into the template. Repeat with
   *Max* (4 codes), then *Colour* with the code filling most of the preview.

Record the camera line from Telemetry (it shows the negotiated size and frame
rate): an iPhone's 60 fps formats are 1920×1440 and larger, which the app
scales to 1280×960, so anything else there means the 60 fps request was
refused.

For reference only, not a hardware result: the same panel fed by a synthetic
camera (`cargo run -p desktop-sender --release --features receive --example
receive_demo`, which films a Max stream of a 1 MiB random file at 1280×960
and 2.55 px per module, missing one frame in ten) received it in 2.0 s at
524 KiB/s average, 618 KiB/s peak, with 57 capture fps and all four codes
locked on an Apple Silicon Mac. A lens, a display and a hand will cost some of
that; how much is what this case measures.

| run | preset | elapsed | average | peak | camera | catch rate | dropped | sha256 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Fast | | | | | | | |
| 2 | Max | | | | | | | |
| 3 | Colour | | | | | | | |

The synthetic figure above was measured on wire v3, before v4. Run 3 tests
colour on the desktop receiver, which unmixes the camera's own NV12 colour.

## Cases 12–16: wire v4 and colour codes

Telemetry's *Format* line says which wire is arriving, and while colour codes
decode, which layers: `colour RGB` is all three, `colour G` green alone.
Record it with every result.

- **12** shows whether v4's lower overhead survives a lens. Telemetry's
  *Overhead* should read 1.00 on v4. On v3 it varies from run to run, from
  about 1.0 to 1.6. Compare elapsed times across the alternating runs, not
  against older sessions.
- **13** shows whether colour survives a real camera at all. If *Format* never
  shows colour, shows only `colour G`, or the rate stays near one code's
  (about 170 KiB/s), the camera's colour processing beat the unmixing. Write down the distance, how
  much of the view the code fills, the sending screen's brightness, and
  whether True Tone or Night Shift was on. Then step closer before giving up.
  The 120 fps capture run catches more of a 60 fps stream if the phone
  delivers it; the *Camera* line says whether it did.
- **14** is Max on v4, to compare with case 10.
- **15** is for large files. In the loopback benchmark v3 needed 1.26–1.99
  frames per block at 5–16 MiB, more as loss rose, while v4 stayed at 1.00.
  Record *Overhead* and elapsed time for both.
- **16** is about memory. The receiving tab holds about four times the file by
  the end. Safari reloads a tab that runs out of memory, which looks like the
  page starting over. Make the 128 MiB file exactly 128 MiB, the limit; both
  senders accept it. The page decodes on its main thread, so expect it to
  freeze for a few seconds as the last frames arrive: that is the three
  RaptorQ blocks decoding. Record whether the file arrived, the elapsed time,
  and how long that pause lasted.

## Recording template

```
date:      2026-__-__
sender:    <os, arch, display refresh rate>
receiver:  <phone model, os version, browser>
payload:   <name, size>
settings:  <preset, or bytes/frame, fps, ecc, codes>
distance:  <roughly, cm>
elapsed:   <Transfer time shown: first accepted QR capture to verified result>
speed:     <Average speed shown, KiB/s; recovered file bytes / elapsed time>
sha256:    <sender>  /  <receiver>   → match / MISMATCH
telemetry: <catch rate, overhead, capture fps, decode/s, dropped %, camera>
notes:     <anything surprising>
```
