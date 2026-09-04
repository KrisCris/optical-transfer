#!/usr/bin/env python3
"""Verify the vendored QR decoder is the artifact we pinned.

This is the only third-party binary that runs in the receiver, and it runs on
the user's camera pixels. A checksum file is not a security boundary on its own
-- anyone who can edit the .wasm can edit the sums beside it -- but it does
catch the failure that actually happens: a partial copy, a mangled line ending,
or a well-meaning refresh from a newer upstream that silently changes decode
behaviour the rest of this repository has been tested against.

Standard library only, so it runs anywhere the rest of the build does.
"""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

VENDOR = Path(__file__).resolve().parent.parent / "vendor" / "decimen-codec"
SUMS = VENDOR / "SHA256SUMS"

# Every file the checksum list must account for. Listed here as well as in
# SHA256SUMS so that deleting a line from the sums cannot quietly stop checking
# a file.
REQUIRED = (
    "decimen_codec.js",
    "decimen_codec.wasm",
    "decimen_codec.d.ts",
    "LICENSE.zxing-cpp",
    "NOTICE.md",
)


def digest(path: Path) -> str:
    sha = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            sha.update(chunk)
    return sha.hexdigest()


def main() -> int:
    if not SUMS.is_file():
        print(f"missing {SUMS}", file=sys.stderr)
        return 1

    expected: dict[str, str] = {}
    for line in SUMS.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        # `shasum -a 256` writes "<hex>  <name>"; tolerate one or two spaces.
        parts = line.split(None, 1)
        if len(parts) != 2:
            print(f"unparseable line in SHA256SUMS: {line!r}", file=sys.stderr)
            return 1
        expected[parts[1].lstrip("*").strip()] = parts[0].lower()

    failures = []
    for name in REQUIRED:
        if name not in expected:
            failures.append(f"{name}: not listed in SHA256SUMS")
            continue
        path = VENDOR / name
        if not path.is_file():
            failures.append(f"{name}: missing from {VENDOR}")
            continue
        actual = digest(path)
        if actual != expected[name]:
            failures.append(f"{name}: expected {expected[name]}, got {actual}")

    for name in sorted(set(expected) - set(REQUIRED)):
        failures.append(f"{name}: listed in SHA256SUMS but not required -- update REQUIRED")

    if failures:
        print("vendored decoder does not match its pinned checksums:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    print(f"vendor: {len(REQUIRED)} files match their pinned checksums")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
