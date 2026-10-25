# Wire v4 and colour codes

Both senders send wire v4 by default, and every receiver still reads
upstream Decimen v0.5.3's wire v3. This note specifies v4 and explains it.
The normative code is `crates/optical-core/src/protocol.rs` (frames) and
`crates/optical-core/src/raptor.rs` (coding), and
`crates/optical-core/tests/wire_v4.rs` pins both.

## What limited v3

A transfer's speed is bytes per code × codes per second × catch rate ÷
overhead. The rebuilt receiver had brought the catch rate close to 1 in the
loopback benchmark. What remained was in the format:

- **Overhead.** v3's LT code, finished by Gaussian elimination, needed
  1.00–1.59 received frames per source block at 1 MiB and 1.26–1.99 at
  5–16 MiB in the loopback runs below. Its elimination only runs once fewer than a thousand
  blocks are unsolved, so the larger the file, the more frames the receiver
  waits for.
- **Header.** 22 of a version 40 code's 2953 bytes.
- **Reach.** Block numbers are 16 bits, so small frames cap out around 30 MB,
  and upstream stops at 64 MiB.

v4 changes the fountain code to RaptorQ (RFC 6330) and shortens the header.
Colour codes are a separate change and can carry either wire. They triple the
bytes per code by putting three QR codes in one.

## Frame

```text
 0  d1 c3        magic, shared with v3
 2  04           version
 3  flags        low nibble critical, as in v3; none defined
 4  checksum     u32 LE, FNV-1a of the whole container; also names the stream
 8  total_len    u32 LE, container bytes
12  symbol id    u24 LE, the RaptorQ encoding symbol id (ESI)
15  block        u8, the RaptorQ source block number (SBN)
16  symbol       the rest of the frame; its length is the symbol size T
```

The container is the same as on v3: gzip where it helps, a SHA-256 over the
file, the name and the media type (`container.rs`). v3 receivers, upstream's
included, see version 4 and say the sender is newer.

## Source blocks and sequencing

Both ends derive the block layout from `(total_len, T)` alone, so no
transmission information travels:

- **Symbol size.** The sender starts from `T_max` = frame bytes − 16. It keeps
  `ceil(total_len / T_max)` symbols but spreads the payload evenly across
  them, `T = ceil(total_len / symbols)`, so a short text rides a small, coarse
  code. v3 sizes its blocks the same way.
- **Blocks.** `Kt = ceil(total_len / T)` source symbols are split into
  `Z = ceil(Kt / 16384)` source blocks by RFC 6330's partition (§4.4.1.2),
  which makes their sizes differ by at most one symbol. There are no
  sub-blocks (`N = 1`) and the alignment is one byte (`Al = 1`). The padded
  payload's last symbol is zero-filled.
- **Order.** Frame `n` carries block `n mod Z`, symbol `(n div Z) mod 2^24`.
  Ids below a block's size `K` are source symbols, which are the payload's
  own bytes. Ids from `K` up are repair symbols. A stream is one systematic
  sweep of `Kt` frames with the blocks interleaved, then fresh repair symbols
  forever. Interleaving spreads a burst of missed frames across blocks.
  Nothing repeats for 2^24 × Z frames, which is over three days of one code
  at 60 fps.

16 384 symbols per block bound the working copy a decode needs and how long
it takes. A full block of 2937-byte symbols is 46 MiB. The RFC allows 56 403.

## Stream identity

A receiver pins the checksum, `total_len`, `T` and the critical flags. There
is no session id. A sender restarted on the same file continues the
receiver's transfer whenever `T` comes out the same. Containers are
byte-for-byte reproducible (ZIP entries carry a fixed 1980 timestamp, and
gzip carries none) and RaptorQ is deterministic, so the same symbol id is the
same bytes. ECC, colour, frame rate and code count never reach the wire, so
changing them keeps the stream. Bytes per frame starts a new one unless the
payload is small enough to give the same `T` anyway. If two different
containers matched all four fields, the mixed symbols would fail verification
and the receiver would say so. Nothing unverified is ever shown.

## Receiving

The version byte picks the decoder: v3's LT or v4's RaptorQ. Everything after
decoding is shared: the frame checksum over the container, then the
container's SHA-256 over the file.

- **Validation before allocation.** `total_len` ≤ 128 MiB plus the
  container's own header, name and media type at their longest, `T` ≤ 65 535,
  `Kt` ≤ 2^19 and `block < Z` are checked before a decoder exists, so a lying
  header cannot claim memory.
- **Decode attempts.** A block first tries to decode once it holds `K`
  symbols. RaptorQ fails there about once in a hundred and at `K + 2` about
  once in a million. After the `f`-th failure it waits `2^f − 1` more symbols
  (1, 3, 7 …, capped at 2^16), so a stream built to defeat the decoder cannot
  buy a full attempt with every frame.
- **Giving up.** A block that has collected `2K + 64` symbols without
  decoding is abandoned with a message. So is a stream past the frame budget
  both wires share, 8 unique frames per source block or symbol.
- **Telemetry.** On v4 the block map has one pixel per source symbol, lit
  when the symbol arrived as itself or its block decoded. *Repair held*
  counts repair symbols held for blocks not yet decoded, and *Decode tries*
  counts attempts.

The tests allow `K + 3` symbols at 5–90% loss. Every loopback run below
decoded from exactly `K`, and so did every run of the study behind this
change, at 5–70% loss.

## Colour codes

`qr::compose` draws three frames of one stream as one code, one per module
byte whose bit `c` is set where the frame in channel `c` (red, green, blue) is
dark. Every frame of a stream has the same version, ECC and pinned mask.
Finder, timing and alignment patterns therefore coincide and stay black and
white, and only data modules take colour. A decoder reading brightness still
finds the code.

A camera mixes the channels: its filters overlap, its colour processing
blends them, and 4:2:0 video keeps colour at half resolution. The three
layers are independent, so the receiver unmixes them blind from the code's own
statistics:

1. Take the covariance of the RGB values over the code's area from about
   2 500 pixels. If its smallest eigenvalue is under 2% of its largest, the
   area is black and white. Black and white through a camera stays under 1%;
   a colour code's three layers give tens of percent. Most crops a receiver
   sees are black and white, and this look costs about 0.02 ms in the web
   worker.
2. Otherwise estimate the covariance again from about 40 000 pixels, and
   treat the area as black and white if the spread is still under 4%.
3. Otherwise whiten it with ZCA, `W = V Λ^-½ Vᵀ`, the decorrelating
   transform nearest the identity. That keeps each layer in its own channel
   and dark modules dark, with no calibration and no pilot symbols.
4. Write each whitened channel as grey (128 + 60 per standard deviation) and
   read it as an ordinary QR code, green first. Green is most of luma, so it
   keeps the most detail through the camera's coarse colour, and where it
   reads, red and blue are read at its position: the web worker's tracked
   path from green's quad, and no full reread of a layer that missed there.
5. A layer that misses is sharpened and read again: each pixel moves 1.2
   times its difference from the mean of a box about two modules wide, at
   the module pitch the code was found or read at. Coarse chroma pulls a lone
   light module among dark ones dark; this pushes it back. Only a layer that
   missed pays for it, about as much as unmixing it did.

A crop around a known code is checked for colour first. A black and white
crop, or a colour one whose layers do not read, goes on to the brightness
reader. Luma weights green most (59% in Rec. 601, 72% in Rec. 709), so a
brightness reader can decode the green layer on its own, which would hide the
other two. A full scan therefore checks every code it found, decoded or not,
for colour, up to four per scan. The web
worker unmixes the canvas's RGBA (`web/app/unmix.js`). The desktop receiver
converts the camera's NV12 chroma plane with BT.709 coefficients
(`receive/unmix.rs`).

Colour needs larger modules than black and white. 4:2:0 alone keeps colour at
half resolution, and a phone's colour noise reduction smears it further. The
first real camera, an iPhone receiving in Safari at 1280 pixels, settled
what the loopback benchmark could not. Version 40 colour codes read nothing
from a laptop screen, where the code was about 2.8 camera pixels per module,
and only slowly from a 27-inch monitor. `scripts/colour-camera.mjs` reproduces
both once chroma is smeared by a Gaussian of about 1 pixel on top of 4:2:0
(`cblur=1`): nothing at 2.8 pixels per module, and at 3.4 only green, 0.88
codes per frame, with the receiver as it was then. The *Colour* preset
therefore uses version 27 codes. With green first and sharpening, the same
model gives, per camera frame (1280×720, blur σ 1 px, 24 frames):

| Code, pixels per module | Reads per frame | Bytes per frame | Web worker per crop |
| --- | --- | --- | --- |
| black and white v40, 2.8 | 1.00 | 2937 | 4.8 ms |
| colour v40, 2.8 | 0 | 0 | — |
| colour v27, 3.9 | 3.00 | 4347 | 12.2 ms |
| colour v40, 3.4 | 2.83 | 8322 | 35.6 ms |
| colour v27, 4.7 | 3.00 | 4347 | 14.6 ms |

At `cblur=2` version 27 keeps green, and red where the code is large, so it
degrades to one or two layers rather than to nothing. Version
40 colour is worth choosing in *Fine-tune* only when the code fills most of
the camera's view. Four black and white codes, at about 2.2 pixels per
module in the same view, still carry the most when the camera is steady
enough for them.

## Limits

| | v4 | v3 |
| --- | --- | --- |
| File | 128 MiB | 64 MiB, as upstream |
| Text | 4 MiB | 4 MiB |
| Source symbols or blocks | 2^19 symbols, 16 384 per block | 65 535 blocks |
| Header | 16 bytes | 22 bytes |

The file limit is on the file a person chooses. Packing adds a header, the
name and the media type, so v4 carries containers up to that much over
128 MiB (`MAX_CONTAINER_BYTES`). v3 limits the container itself to 64 MiB,
as upstream's receivers do. A file within a few hundred bytes of that which
does not compress is refused before it starts, with a message suggesting v4.

128 MiB is a memory budget, not a format limit. v4 could describe 4 GiB. A
receiving browser tab holds the symbols, one block's decoding workspace, the
assembled container and the unpacked file at once, roughly four times the
file. A phone is the tightest place this runs, and 128 MiB has not been tried
on one.

## Measurements

`scripts/loopback-bench.mjs`: headless Chrome re-films the browser sender's
own display through a simulated 1280×960 camera into the real capture and
decode path. The camera applies blur σ 0.6 px, 2° tilt, contrast 0.8 and
brightness 0.95. Colour runs add `chroma=1`: crosstalk 0.2, 4:2:0 chroma and
noise of 4 grey levels. Files are incompressible, at 2953 bytes per code and
60 fps.

*Before* is this repository at the commit before v4 (wire v3 only), built
separately and served next to the current build. Both ran the same benchmark
page, interleaved, on an 8-core Apple Silicon Mac that other work was also
using. The 1 MiB figures count runs whose page captured at 50 fps or more.
Rates are KiB/s; *frames per block* is unique frames used per source block or
symbol.

| 1 MiB | Runs | Time, s | Rate | Catch rate | Frames per block |
| --- | --- | --- | --- | --- | --- |
| 1 code, before (v3) | 7 | 6.07–10.19, median 6.90 | 101–169, median 148 | 0.95–1.00 | 1.02–1.59 |
| 1 code, current receiver on v3 | 7 | 6.00–8.40, median 7.12 | 122–171, median 144 | 0.86–1.00 | 1.00–1.23 |
| 1 code, v4 | 6 | 6.00–6.32, median 6.10 | 162–171, median 168 | 0.96–0.99 | 1.00 |
| 1 colour code, v4 | 6 | 2.07–2.30, median 2.10 | 445–495, median 489 | 0.87–0.97 | 1.00 |
| 4 codes, before (v3) | 6 | 1.84–2.35, median 1.98 | 436–556, median 518 | 0.64–0.96 | 1.01–1.32 |
| 4 codes, v4 | 7 | 1.54–2.29, median 1.70 | 447–665, median 601 | 0.65–0.97 | 1.00 |

The current receiver reads v3 as fast as the old one did. The difference
between those two rows is v3's own overhead, which varies more from run to
run than the builds differ: in the four quietest pairs the medians were 155
and 153 KiB/s. At four codes a 1 MiB transfer lasts about two seconds, much of
it spent locking onto the codes, so catch rates and rates spread widely.

Larger files, every run, as time (catch rate, frames per block). v4 used
1.00 frames per block in every run, so its cells leave that out:

| File, codes | Before (v3) | v4 |
| --- | --- | --- |
| 5 MiB, 1 | 40.9 s (0.96, 1.30); 47.4 s (0.93, 1.41) | 31.9 s (0.94); 34.3 s (0.90) |
| 5 MiB, 4 | 52.8 s (0.35, 1.26); 43.1 s (0.35, 1.27) | 36.0 s (0.32); 27.1 s (0.35) |
| 16 MiB, 4 | 32.3 s (0.97, 1.31); 33.6 s (0.95, 1.33); 45.9 s (0.68, 1.28); 70.2 s (0.69, 1.92); 81.2 s (0.59, 1.99); 91.9 s (0.57, 1.91) | 24.3 s (0.98); 24.8 s (0.97); 30.3 s (0.79); 56.1 s (0.50); 60.5 s (0.44); 63.4 s (0.45); 76.8 s (0.50); 98.3 s (0.41) |

At 16 MiB and four codes, how much of the stream the page caught depended on
what else the machine was doing, and that spread the times fourfold. The
one-minute load average ran from 8 to 81 during these runs. With the
machine quiet, both wires caught 97–98% of frames, and v4 finished in 24.3
and 24.8 s, 98% and 96% of the wire rate. v3's two such runs took 32.3 and 33.6 s.
In the other interleaved runs v4 mostly caught less. A one-minute load
average cannot say what happened within a run, so four checks separated the
wire from the load:

- **Side by side.** Each wire ran in its own headless Chrome at the same
  time, so both saw the same load, 40–47. From second 3 to 24 of each of
  four pairs, v4 and v3 caught a median 0.37 and 0.38 of frames, then 0.36
  and 0.38, 0.37 and 0.36, and 0.39 and 0.40.
- **Sending.** The browser's wasm sender takes 0.81 ms per code on either
  wire, both during v4's systematic sweep and after it.
- **Reading.** zxing reads either wire's codes equally fast: 6.1 and 6.0 ms
  per clean version 40 code, and 8.1 and 8.2 ms blurred.
- **Receiving.** Natively, `ReceiverState::add_frame` takes 0.3 µs per v4
  frame (median; 0.6 µs at the 99th percentile), against 6.1 µs (18.5 µs) per
  v3 frame. The frame that completes a 16 MiB transfer at 50% loss takes
  114 ms on v4, decode and verification included.

None of these leaves the wire a way to catch fewer frames, and the one
comparison in which both wires saw the same load shows that it does not.

At 5 MiB and four codes the pairs ran at matching catch rates of 0.32–0.35.
There v4 took 0.63–0.68 of v3's time, more than its overhead alone explains.
v3's carousel repeats, so a receiver that misses most frames starts catching
ones it already has. v4's repair symbols never repeat. Colour at 5 MiB and
1920×1080 ran only under load, catching 45% and 49% of frames, and still
finished in 23.8 and 24.8 s, against 31.9–34.3 s for one black and white v4
code.

In the web worker a colour crop took 16–20 ms at one code (all three layers
unmixed and read), against 2.5–3 ms for a black and white crop. At the
*Colour* preset's 1465 bytes the loopback benchmark ran at 249 and 252 KiB/s
of the 255 on the wire, catching 97–99% of frames, with crops at 10–11 ms.

The coding itself, natively, with this crate's `RaptorEncoder` and
`RaptorDecoder`, best of three on the same machine: at 50% loss a block of
1024 symbols of 2937 bytes decodes in 9 ms, a 16 MiB file's 5713 in 63 ms
and a full block of 16 384 in 214 ms. At 5% loss each is up to 15% faster.
Setting up the encoder takes a little longer: 71 ms for 16 MiB and 273 ms
for a full block. A single run in a busier minute took 1.06 s for the full
block. As WebAssembly, run in Node on the browser's engine, the sender
prepares a 16 MiB file in 1.1–1.9 s on v4, against 0.5–0.9 s on v3, and a
128 MiB one in 9.6 s; in the app that happens in its worker. The receiver decodes on the page's main
thread. In a 16 MiB loopback run the frame that completed the transfer held
that thread for 0.8 s, with the decode, both checksums, unpacking and the
benchmark's own comparison of the bytes. At 128 MiB three blocks complete
within a few frames of each other, so the page will pause for a few seconds
at the end. Moving the decoder into a worker would remove the pause if it
matters on phones.

## What is not verified

Colour has met one real camera, an iPhone, only with version 40 codes and
the receiver before green-first reading and sharpening; the *Colour* preset's
version 27 codes have not. The loopback camera models blur, contrast loss,
linear crosstalk, 4:2:0 chroma and noise, and `scripts/colour-camera.mjs`
adds a phone's further colour smear. Neither models a phone's white balance
and saturation, a screen's gamut, glare, moiré, or rolling shutter. `tests/end-to-end/README.md` lists the runs to make.
