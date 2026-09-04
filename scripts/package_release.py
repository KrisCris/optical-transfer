#!/usr/bin/env python3
"""Assemble one platform's release archive.

Deliberately Python and not a shell script: this has to produce identical
layouts on Windows, macOS and Linux runners, and the shell does not.

The release carries the sender, the whole receiver bundle, and the legal files
that make distributing an AGPL derivative lawful. SOURCE.txt is written here
rather than checked in because it has to name the *commit being packaged* --
a stale source pointer is worse than none, since it looks like an answer.

    python3 scripts/package_release.py \
      --target aarch64-apple-darwin \
      --binary target/release/desktop-sender
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.strip()


def digest(path: Path) -> str:
    sha = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            sha.update(chunk)
    return sha.hexdigest()


def copy_receiver(destination: Path) -> None:
    source = ROOT / "web" / "receiver"
    for required in ("pkg/optical_wasm_bg.wasm", "vendor/decimen-codec/decimen_codec.wasm"):
        if not (source / required).is_file():
            raise SystemExit(
                f"{source / required} is missing -- run scripts/build-web.sh first"
            )
    # The local test harness is not part of a release: it exists to check the
    # build, and shipping it would put a page that fetches fixtures into the
    # service worker's scope.
    shutil.copytree(
        source,
        destination,
        ignore=shutil.ignore_patterns("tests", ".gitignore", "*.map"),
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, help="rust target triple")
    parser.add_argument("--binary", required=True, type=Path, help="built sender executable")
    parser.add_argument("--out", type=Path, default=ROOT / "dist")
    parser.add_argument(
        "--allow-dirty",
        action="store_true",
        help="package anyway with an uncommitted tree (the archive will say so)",
    )
    args = parser.parse_args()

    dirty = bool(git("status", "--porcelain"))
    if dirty and not args.allow_dirty:
        print(
            "the working tree has uncommitted changes, so SOURCE.txt could not\n"
            "honestly name the source for this build. Commit, or pass --allow-dirty.",
            file=sys.stderr,
        )
        return 1

    binary = args.binary.resolve()
    if not binary.is_file():
        print(f"{binary} does not exist -- build the sender first", file=sys.stderr)
        return 1

    staging = args.out / args.target
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir(parents=True)

    shutil.copy2(binary, staging / binary.name)
    (staging / binary.name).chmod(0o755)
    for name in ("LICENSE", "NOTICE.md", "README.md"):
        shutil.copy2(ROOT / name, staging / name)
    copy_receiver(staging / "receiver")

    commit = git("rev-parse", "HEAD")
    (staging / "SOURCE.txt").write_text(
        "Optical Transfer — corresponding source\n"
        "=======================================\n\n"
        f"target:     {args.target}\n"
        f"commit:     {commit}{' (built from a dirty tree)' if dirty else ''}\n"
        "repository: see NOTICE.md for the upstream this is derived from\n\n"
        "This program is licensed AGPL-3.0-or-later. You are entitled to the\n"
        "complete corresponding source for this build, including the source of\n"
        "the vendored decimen-codec decoder in receiver/vendor/. If you received\n"
        "this archive without a way to obtain that source, whoever gave it to you\n"
        "has not met the licence's terms — ask them for it.\n\n"
        "Upstream baseline: Decimen Optical Transfer v0.5.3,\n"
        "commit 5301fc2020716b5a97d4edf6e1793c3dedbf6e17.\n"
    )

    # Checksums over everything except the checksum file itself.
    entries = sorted(
        path for path in staging.rglob("*") if path.is_file() and path.name != "SHA256SUMS"
    )
    (staging / "SHA256SUMS").write_text(
        "".join(f"{digest(path)}  {path.relative_to(staging).as_posix()}\n" for path in entries)
    )

    archive = args.out / f"optical-transfer-{args.target}.zip"
    archive.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as bundle:
        for path in sorted(staging.rglob("*")):
            if path.is_file():
                bundle.write(path, Path(args.target) / path.relative_to(staging))

    print(f"staged  {staging}")
    print(f"archive {archive} ({archive.stat().st_size / 1024 / 1024:.1f} MB)")
    print(f"sha256  {digest(archive)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
