# Cross-implementation conformance

Everything else in this repository checks the port against constants copied out
of upstream, or against itself. Both can be self-consistently wrong. These two
scripts check it against the upstream implementation *running*, in both
directions, and they are the only evidence that the gzip layer interoperates at
all: `flate2` and the browser's `CompressionStream` are different deflate
implementations and will never agree byte for byte on what they emit, so the
only claim worth testing is that each can read the other's output.

Both scripts run inside a checkout of the pinned baseline — Decimen Optical
Transfer `v0.5.3`, commit `5301fc2020716b5a97d4edf6e1793c3dedbf6e17` — because
they import its own `shared/*.ts` directly. Nothing here is a dependency of the
Rust build; it is a periodic and pre-baseline-bump check.

```sh
# 0. Get the pinned baseline and its dev dependencies.
git clone https://github.com/bashalarmistalt/decimen-optical-transfer upstream
git -C upstream checkout 5301fc2020716b5a97d4edf6e1793c3dedbf6e17
npm --prefix upstream ci

# 1. Browser -> Rust. Regenerates the checked-in golden vectors.
cp scripts/upstream-conformance/emit-vectors.ts upstream/
(cd upstream && ./node_modules/.bin/tsx emit-vectors.ts \
   ../crates/optical-core/tests/vectors/upstream-v0.5.3.txt)
cargo test -p optical-core --test upstream_conformance

# 2. Rust -> browser. No checked-in artifact; the upstream unpacker is the check.
cargo run -q -p optical-core --example emit_containers > /tmp/rust-containers.txt
cp scripts/upstream-conformance/read-back.ts upstream/
(cd upstream && ./node_modules/.bin/tsx read-back.ts /tmp/rust-containers.txt)
```

A diff in step 1's output is a wire-format change, not a constant to re-record.
Step 2 exits non-zero on any disagreement.

Last run: 2026-09-04 against `5301fc2`, Node 24.9.0 — 4/4 vectors and 5/5
read-backs green, covering an empty-defaulted media type, a path-traversal
filename, browser-gzipped and Rust-gzipped payloads, an incompressible payload
that both sides decline to compress, and a lossy upstream frame stream decoded
by the Rust fountain.
