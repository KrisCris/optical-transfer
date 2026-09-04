# Optical Transfer Rust Port

This project ports Decimen Optical Transfer v0.5.3 to a shared Rust optical
transfer core. The Cargo workspace contains `optical-core`, which owns the
wire-v3 framing, fountain codec, and verified file containers. Future native
and WebAssembly adapters consume that same byte-exact core.

## Security

Optical transfer is not encrypted. Any camera pointed at the displayed QR
stream can read the transfer. Received data must pass both the optical-frame
checksum and file SHA-256 verification before it is exposed.

## Build prerequisites

- Rust and Cargo 1.98.1
- The `wasm32-unknown-unknown` target
- `wasm-pack` 0.15.0 for future WebAssembly packaging

The checked-in `rust-toolchain.toml` installs the Rust components and target.

## License and attribution

This is an AGPL-3.0-or-later derivative of Decimen Optical Transfer v0.5.3
(commit `5301fc2020716b5a97d4edf6e1793c3dedbf6e17`). See
`LICENSE` and `NOTICE.md` for the full AGPL attribution and zxing-cpp
Apache-2.0 notice.
