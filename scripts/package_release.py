#!/usr/bin/env python3
"""Assemble one platform's release archive.

Deliberately Python and not a shell script: this has to produce identical
layouts on Windows, macOS and Linux runners, and the shell does not.

The release carries the sender, the whole web bundle (receiver and browser sender), and the legal files
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

# Kept in step with the workspace manifest by hand; there is one number and it
# only moves on a release.
VERSION = "0.1.0"


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


APP_NAME = "Optical Transfer"
BUNDLE_ID = "org.opticaltransfer.sender"
CAMERA_USAGE = "Optical Transfer reads the QR codes another screen shows to receive a file."


def make_macos_bundle(staging: Path, binary: Path, version: str) -> Path:
    """Wrap the sender in a .app so macOS treats it as an application.

    A bare Mach-O binary is not an application on this platform. Double-clicked
    from Finder it opens a Terminal window instead of the program; it gets no
    Dock icon, no bundle identifier, and no place in the app switcher. None of
    that is cosmetic — a user who downloads a zip and double-clicks the file
    inside it should get the sender, not a shell.
    """
    app = staging / f"{APP_NAME}.app"
    macos = app / "Contents" / "MacOS"
    macos.mkdir(parents=True)
    shutil.copy2(binary, macos / "desktop-sender")
    (macos / "desktop-sender").chmod(0o755)
    (app / "Contents" / "Info.plist").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
        '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
        '<plist version="1.0">\n'
        "<dict>\n"
        "  <key>CFBundleName</key><string>" + APP_NAME + "</string>\n"
        "  <key>CFBundleDisplayName</key><string>" + APP_NAME + "</string>\n"
        "  <key>CFBundleIdentifier</key><string>" + BUNDLE_ID + "</string>\n"
        "  <key>CFBundleExecutable</key><string>desktop-sender</string>\n"
        "  <key>CFBundlePackageType</key><string>APPL</string>\n"
        "  <key>CFBundleVersion</key><string>" + version + "</string>\n"
        "  <key>CFBundleShortVersionString</key><string>" + version + "</string>\n"
        "  <key>LSMinimumSystemVersion</key><string>11.0</string>\n"
        # The sender's whole job is to be looked at, so it must not be
        # eligible for App Nap or display sleep while it is streaming.
        "  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>\n"
        "  <key>NSHighResolutionCapable</key><true/>\n"
        # Receive mode reads a camera. Without a usage string macOS kills the
        # app the moment it asks; without the Continuity key an iPhone shows
        # up only under the generic external-camera type, if at all.
        "  <key>NSCameraUsageDescription</key><string>"
        + CAMERA_USAGE
        + "</string>\n"
        "  <key>NSCameraUseContinuityCameraDeviceType</key><true/>\n"
        "</dict>\n"
        "</plist>\n"
    )
    return app


def copy_web(destination: Path) -> None:
    source = ROOT / "web" / "app"
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

    changes = git("status", "--porcelain")
    dirty = bool(changes)
    if dirty and not args.allow_dirty:
        print(
            "the working tree has uncommitted changes, so SOURCE.txt could not\n"
            "honestly name the source for this build. Commit, or pass --allow-dirty.\n"
            f"{changes}",
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

    if "apple-darwin" in args.target:
        make_macos_bundle(staging, binary, VERSION)
    else:
        shutil.copy2(binary, staging / binary.name)
        (staging / binary.name).chmod(0o755)
    for name in ("LICENSE", "NOTICE.md", "README.md"):
        shutil.copy2(ROOT / name, staging / name)
    copy_web(staging / "web")

    commit = git("rev-parse", "HEAD")
    (staging / "SOURCE.txt").write_text(
        "Optical Transfer — corresponding source\n"
        "=======================================\n\n"
        f"target:     {args.target}\n"
        f"commit:     {commit}{' (built from a dirty tree)' if dirty else ''}\n"
        "repository: see NOTICE.md for the upstream this is derived from\n\n"
        "This program is licensed AGPL-3.0-or-later. You are entitled to the\n"
        "complete corresponding source for this build, including the source of\n"
        "the vendored decimen-codec decoder in web/vendor/. If you received\n"
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
