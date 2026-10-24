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
4. Every module parses. One syntax error in an imported module takes the
   whole page down with it, including the half the user did not touch. Needs
   Node; skipped with a note when it is not installed.

Standard library only, and deliberately blunt: it reads files as text and looks
for patterns rather than parsing HTML, because a parser dependency here would
itself be something to keep pinned.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RECEIVER = ROOT / "web" / "app"
SERVICE_WORKER = RECEIVER / "service-worker.js"

# Element ids the modules bind to. Listed here so a rename in one place fails
# the build instead of producing a page with a dead button.
REQUIRED_IDS = (
    "mode-receive", "mode-send", "panel-receive", "panel-send",
    "rx-viewfinder", "rx-video", "rx-overlay", "rx-hud-state", "rx-hud-rate", "rx-hud-bar",
    "rx-start", "rx-stop", "rx-settings", "rx-camera", "rx-width", "rx-workers",
    "rx-status", "rx-error", "rx-hint", "rx-phase", "rx-percent", "rx-speed", "rx-eta",
    "rx-elapsed", "rx-progress", "rx-bar", "rx-chart", "rx-chart-scale", "rx-map", "rx-map-label",
    "rx-result", "rx-result-name", "rx-result-meta", "rx-result-image", "rx-result-text",
    "rx-save", "rx-share", "rx-copy", "rx-again",
    "t-size", "t-k", "t-blocklen", "t-frames", "t-redundant", "t-pending", "t-elim",
    "t-catch", "t-overhead", "t-avgpeak", "t-capture", "t-decode", "t-symbols", "t-codes",
    "t-mode", "t-workers", "t-tracked", "t-latency", "t-dropped", "t-camera", "t-decoder",
    "tx-files-panel", "tx-text-panel", "tx-drop", "tx-files", "tx-folder", "tx-folder-button",
    "tx-summary", "tx-clear", "tx-list", "tx-text", "tx-text-count",
    "tx-bytes", "tx-fps", "tx-codes", "tx-ecc", "tx-preset-rate",
    "tx-start", "tx-stop", "tx-status", "tx-error",
    "tx-stage", "tx-canvas", "tx-fullscreen", "tx-exit",
    "s-session", "s-cycle-bar", "s-cycle", "s-payload", "s-compression", "s-code", "s-k",
    "s-rate", "s-ideal", "s-frames", "s-flips", "s-display",
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
    # Modules and workers are imported from script, not markup: every one of
    # them has to be precached too, or the app loads offline and then fails.
    for module in sorted(RECEIVER.glob("*.js")):
        if module.name != "service-worker.js" and module.name not in precached:
            failures.append(f"{module.name} is not precached by the service worker")


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


def check_modules_parse(failures: list[str]) -> bool:
    node = shutil.which("node")
    if not node:
        return False
    modules = sorted(RECEIVER.glob("*.js")) + sorted((RECEIVER / "tests").glob("*.mjs"))
    with tempfile.TemporaryDirectory() as scratch:
        for module in modules:
            # Copied to .mjs so Node parses it as the browser will: as a module.
            copy = Path(scratch) / f"{module.stem}.mjs"
            shutil.copyfile(module, copy)
            result = subprocess.run([node, "--check", str(copy)], capture_output=True, text=True)
            if result.returncode != 0:
                # The temp path may be reported through a symlink (/private/var).
                detail = re.sub(r"\S*" + re.escape(copy.name), str(module.relative_to(ROOT)),
                                result.stderr.strip())
                failures.append(f"{module.relative_to(ROOT)} does not parse:\n{detail}")
    return True


def check_structure(failures: list[str]) -> None:
    html = (RECEIVER / "index.html").read_text()
    for element_id in REQUIRED_IDS:
        if f'id="{element_id}"' not in html:
            failures.append(f"index.html has no element with id={element_id!r}")
    if "playsinline" not in html:
        failures.append("the video element needs playsinline or iOS opens it fullscreen")
    if 'role="alert"' not in html or 'role="status"' not in html:
        failures.append("status and error regions must be announced to a screen reader")
    for element_id in ("tx-files", "tx-folder"):
        tag = re.search(rf'<input\b[^>]*id="{element_id}"[^>]*>', html)
        if not tag or not re.search(r'\bmultiple\b', tag.group()):
            failures.append(f"{element_id} must allow multiple files")
        if element_id == "tx-folder" and (not tag or "webkitdirectory" not in tag.group()):
            failures.append("tx-folder must use the directory picker")
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
    parsed = check_modules_parse(failures)

    if failures:
        print("receiver contract violations:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    print(f"web: {len(assets)} precached assets, no runtime network, structure intact"
          + (", modules parse" if parsed else " (node not found: module parse check skipped)"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
