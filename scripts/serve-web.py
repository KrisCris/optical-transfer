#!/usr/bin/env python3
"""Serve the receiver over HTTPS for phone testing.

Not a production server, and it says so. It exists because `getUserMedia` is
gated on a secure context: plain http://<laptop-ip>:8000 will load the page on
a phone and then silently refuse the camera, which looks like a broken
receiver rather than a browser policy. localhost is exempt from that rule, but
localhost is not where the phone is.

Cross-origin isolation headers are set so the page keeps working if it ever
needs SharedArrayBuffer for the decode workers. Cache-Control is off because a
dev server that caches is a dev server that shows you yesterday's build.

Standard library only.

    python3 scripts/serve-web.py --directory web/app --port 8443 \
      --cert .tooling/dev-cert.pem --key .tooling/dev-key.pem
"""

from __future__ import annotations

import argparse
import functools
import http.server
import socket
import ssl
import sys
from pathlib import Path

# Served so the browser treats them as modules and WebAssembly, not text.
EXTRA_TYPES = {
    ".js": "text/javascript",
    ".mjs": "text/javascript",
    ".wasm": "application/wasm",
    ".webmanifest": "application/manifest+json",
    ".svg": "image/svg+xml",
}


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, **EXTRA_TYPES}

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        super().end_headers()

    def log_message(self, fmt: str, *args: object) -> None:
        sys.stderr.write(f"{self.address_string()} {fmt % args}\n")


def local_addresses(port: int, scheme: str) -> list[str]:
    """Best-effort list of URLs a phone on the same network can reach."""
    urls = [f"{scheme}://localhost:{port}/"]
    try:
        # Connecting a UDP socket sends nothing but picks the outbound
        # interface, which is the address the phone will be able to see.
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
            probe.connect(("192.0.2.1", 1))  # TEST-NET-1: routable to nowhere
            urls.append(f"{scheme}://{probe.getsockname()[0]}:{port}/")
    except OSError:
        pass
    return urls


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", default="web/app")
    parser.add_argument("--port", type=int, default=8443)
    parser.add_argument("--cert", type=Path)
    parser.add_argument("--key", type=Path)
    args = parser.parse_args()

    directory = Path(args.directory).resolve()
    if not (directory / "index.html").is_file():
        print(f"{directory} has no index.html -- run scripts/build-web.sh first", file=sys.stderr)
        return 1

    server = http.server.ThreadingHTTPServer(
        ("0.0.0.0", args.port),
        functools.partial(Handler, directory=str(directory)),
    )

    scheme = "http"
    if args.cert and args.key:
        if not args.cert.is_file() or not args.key.is_file():
            print("the certificate or key is missing. Create a throwaway pair with:", file=sys.stderr)
            print(
                "  openssl req -x509 -newkey rsa:2048 -nodes -days 7 \\\n"
                "    -keyout .tooling/dev-key.pem -out .tooling/dev-cert.pem -subj '/CN=localhost'",
                file=sys.stderr,
            )
            return 1
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(args.cert, args.key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
        scheme = "https"
    else:
        print(
            "No certificate given, serving plain HTTP. The camera will only work on\n"
            "localhost -- a phone will load the page and then refuse getUserMedia.",
            file=sys.stderr,
        )

    for url in local_addresses(args.port, scheme):
        print(f"serving {directory} at {url}")
    if scheme == "https":
        print("The certificate is self-signed; the phone will ask you to trust it once.")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
