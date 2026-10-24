#!/bin/sh
# The one entry point for building the served receiver tree.
#
# There is deliberately no other way to assemble web/app/: two ways to
# build it is two trees that can differ, and the one that differs is always the
# one that shipped.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

echo "==> building the Rust sender and receiver"
wasm-pack build crates/optical-wasm --target web --out-dir ../../web/app/pkg

echo "==> verifying the vendored QR decoder"
python3 scripts/check-vendor.py

echo "==> copying the vendored QR decoder into the served tree"
mkdir -p web/app/vendor/decimen-codec
cp vendor/decimen-codec/decimen_codec.js \
   vendor/decimen-codec/decimen_codec.wasm \
   vendor/decimen-codec/NOTICE.md \
   vendor/decimen-codec/LICENSE.zxing-cpp \
   web/app/vendor/decimen-codec/

echo "==> checking the receiver's static contract"
python3 scripts/check-web.py

echo "==> web/app is ready to serve"
