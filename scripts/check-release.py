#!/usr/bin/env python3
"""Assert an assembled release directory is complete and legally distributable.

Run against `dist/<target>/` before anything is uploaded. Two classes of
failure it exists to prevent:

* A build that ships without its source offer. This project is a derivative of
  an AGPL-3.0-or-later work, so a binary without LICENSE, NOTICE.md and a
  pointer to corresponding source is not a release -- it is a licence
  violation. Making that a build failure is cheaper than remembering.
* A web bundle missing one of its two WebAssembly modules, or a module its
  service worker precaches. It would load, reach the camera screen, and decode
  nothing -- or work online and fail offline -- which is the hardest kind of
  failure to attribute.

Standard library only.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from pathlib import Path

# Files that must exist in every release, whatever the platform. The web
# bundle's modules are not listed here: whatever its service worker precaches
# must be present, which keeps this list from going stale on every rename.
REQUIRED_FILES = (
    "LICENSE",
    "NOTICE.md",
    "SOURCE.txt",
    "SHA256SUMS",
    "README.md",
    "web/index.html",
    "web/service-worker.js",
    "web/pkg/optical_wasm.js",
    "web/pkg/optical_wasm_bg.wasm",
    "web/vendor/decimen-codec/decimen_codec.js",
    "web/vendor/decimen-codec/decimen_codec.wasm",
    "web/vendor/decimen-codec/LICENSE.zxing-cpp",
    "web/vendor/decimen-codec/NOTICE.md",
)

# The sender, named per platform. Exactly one must be present.
SENDER_CANDIDATES = (
    "desktop-sender",
    "desktop-sender.exe",
    "Optical Transfer.app/Contents/MacOS/desktop-sender",
)


def digest(path: Path) -> str:
    sha = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            sha.update(chunk)
    return sha.hexdigest()


def precached(root: Path) -> list[str]:
    worker = root / "web" / "service-worker.js"
    if not worker.is_file():
        return []
    match = re.search(r"const ASSETS\s*=\s*\[(.*?)\];", worker.read_text(), re.S)
    if not match:
        return []
    assets = re.findall(r"[\"']([^\"']+)[\"']", match.group(1))
    return [f"web/{asset.removeprefix('./')}" for asset in assets if asset not in ("./", "/")]


def check(root: Path) -> list[str]:
    failures: list[str] = []

    for name in (*REQUIRED_FILES, *precached(root)):
        if not (root / name).is_file():
            failures.append(f"missing {name}")
    if (root / "web" / "tests").exists():
        failures.append("web/tests is a development harness and must not ship")

    senders = [name for name in SENDER_CANDIDATES if (root / name).is_file()]
    if not senders:
        failures.append(
            "no sender executable: expected one of " + ", ".join(SENDER_CANDIDATES)
        )

    source = root / "SOURCE.txt"
    if source.is_file():
        text = source.read_text()
        for token in ("repository", "commit", "AGPL"):
            if token.lower() not in text.lower():
                failures.append(f"SOURCE.txt does not mention {token}")

    notice = root / "NOTICE.md"
    if notice.is_file():
        text = notice.read_text()
        for token in ("5301fc2020716b5a97d4edf6e1793c3dedbf6e17", "AGPL-3.0-or-later", "zxing-cpp"):
            if token not in text:
                failures.append(f"NOTICE.md does not carry {token}")

    # Every checksum listed must match, and every listed file must exist. A
    # SHA256SUMS that describes a file nobody shipped is worse than none.
    sums = root / "SHA256SUMS"
    if sums.is_file():
        listed = 0
        for line in sums.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(None, 1)
            if len(parts) != 2:
                failures.append(f"unparseable SHA256SUMS line: {line!r}")
                continue
            expected, name = parts[0].lower(), parts[1].lstrip("*").strip()
            path = root / name
            if not path.is_file():
                failures.append(f"SHA256SUMS lists {name}, which is not in the release")
                continue
            if digest(path) != expected:
                failures.append(f"{name}: checksum does not match SHA256SUMS")
            listed += 1
        if listed == 0:
            failures.append("SHA256SUMS is empty")

    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="an assembled release directory")
    args = parser.parse_args()

    if not args.directory.is_dir():
        print(f"{args.directory} is not a directory", file=sys.stderr)
        return 1

    failures = check(args.directory)
    if failures:
        print(f"{args.directory} is not a complete release:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    print(f"{args.directory}: release contract satisfied")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
