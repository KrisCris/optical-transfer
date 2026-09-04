#!/usr/bin/env python3
"""Static contract checks for the receiver bundle.

Three properties, none of which a browser will complain about until it is too
late to matter:

1. Every asset the service worker promises to precache exists. A precache list
   that names a missing file makes `cache.addAll` reject, which does not break
   the page online -- it breaks it *offline*, later, in someone's hands.
2. Nothing loads over the network at runtime. This application's entire claim
   is that no network sits between the two devices; a receiver that pulls a
   font or an analytics script would make that claim false in the one place a
   user cannot check it.
3. The structural pieces the app script binds to are actually in the HTML.

Standard library only, and deliberately blunt: it reads files as text and looks
for patterns rather than parsing HTML, because a parser dependency here would
itself be something to keep pinned.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RECEIVER = ROOT / "web" / "receiver"
SERVICE_WORKER = RECEIVER / "service-worker.js"

# Element ids app.js binds to. Listed here so a rename in one place fails the
# build instead of producing a receiver with a dead button.
REQUIRED_IDS = (
    "video", "overlay", "viewfinder", "start", "stop", "status", "error",
    "progress", "bar", "percent", "progress-label", "blocks-grid",
    "m-frames", "m-blocks", "m-decodes",
    "result", "result-name", "result-stats", "result-text",
    "result-save", "result-copy", "dismiss",
)

TEXT_SUFFIXES = {".html", ".css", ".js", ".webmanifest", ".json", ".svg"}

# A scheme inside one of these is not a fetch. `xmlns` is the load-bearing one:
# every SVG in existence declares http://www.w3.org/2000/svg, and a check that
# fails on it is a check people learn to skip.
ALLOWED_URL_CONTEXTS = (
    "http://www.w3.org/",          # XML namespaces
    "https://www.w3.org/",
    "http://purl.org/",            # RDF vocabularies in SVG metadata
    "https://github.com/",         # attribution links inside comments
    "https://emscripten.org",
    "https://developer.mozilla.org",
)

URL_PATTERN = re.compile(r"https?://[^\s\"'`)>\\]+")


def parse_precache(source: str) -> list[str]:
    match = re.search(r"const ASSETS\s*=\s*\[(.*?)\];", source, re.S)
    if not match:
        raise SystemExit("service-worker.js has no ASSETS array")
    return re.findall(r"[\"']([^\"']+)[\"']", match.group(1))


def check_precache(failures: list[str]) -> list[str]:
    assets = parse_precache(SERVICE_WORKER.read_text())
    for asset in assets:
        if asset in ("./", "/"):
            continue
        if not (RECEIVER / asset.removeprefix("./")).is_file():
            failures.append(f"service worker precaches {asset}, which does not exist")
    return assets


def check_local_references(failures: list[str], assets: list[str]) -> None:
    """Every script/style/manifest/icon the HTML pulls in must be precached."""
    html = (RECEIVER / "index.html").read_text()
    referenced = set(re.findall(r"(?:src|href)=[\"']([^\"']+)[\"']", html))
    precached = {asset.removeprefix("./") for asset in assets}
    for reference in referenced:
        if reference.startswith(("#", "data:", "mailto:")):
            continue
        if re.match(r"^[a-z]+:", reference):
            failures.append(f"index.html references an absolute URL: {reference}")
            continue
        if reference.startswith("/"):
            failures.append(
                f"index.html references a root-absolute path: {reference} -- "
                "the receiver must work from any subdirectory"
            )
            continue
        target = reference.removeprefix("./")
        if not (RECEIVER / target).is_file():
            failures.append(f"index.html references {reference}, which does not exist")
        elif target not in precached:
            failures.append(f"index.html references {reference}, which the service worker does not precache")


def check_no_runtime_network(failures: list[str]) -> None:
    for path in sorted(RECEIVER.rglob("*")):
        if not path.is_file() or path.suffix not in TEXT_SUFFIXES:
            continue
        if "tests" in path.relative_to(RECEIVER).parts:
            continue  # the local test harness is not shipped
        for line_number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            for url in URL_PATTERN.findall(line):
                if url.startswith(ALLOWED_URL_CONTEXTS):
                    continue
                failures.append(
                    f"{path.relative_to(ROOT)}:{line_number} reaches the network at runtime: {url}"
                )


def check_structure(failures: list[str]) -> None:
    html = (RECEIVER / "index.html").read_text()
    for element_id in REQUIRED_IDS:
        if f'id="{element_id}"' not in html:
            failures.append(f"index.html has no element with id={element_id!r}")
    if "playsinline" not in html:
        failures.append("the video element needs playsinline or iOS opens it fullscreen")
    if 'role="alert"' not in html or 'role="status"' not in html:
        failures.append("status and error regions must be announced to a screen reader")
    if "not encrypted" not in html.lower():
        failures.append("the security warning is missing from index.html")
    # Inline SVG icons are fine; an inline <style> or <script> is not, because
    # the service worker precaches files and cannot version what lives in the
    # HTML's own body.
    if "<style" in html:
        failures.append("index.html has an inline <style>; styles belong in styles.css")


def main() -> int:
    if not RECEIVER.is_dir():
        print(f"missing {RECEIVER}", file=sys.stderr)
        return 1

    failures: list[str] = []
    assets = check_precache(failures)
    check_local_references(failures, assets)
    check_no_runtime_network(failures)
    check_structure(failures)

    if failures:
        print("receiver contract violations:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    print(f"web: {len(assets)} precached assets, no runtime network, structure intact")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
