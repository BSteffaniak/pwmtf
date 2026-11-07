#!/usr/bin/env python3
"""Serve an already verified PWMTF WASM bundle from an in-memory allowlist."""

from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import re
import ssl
from pathlib import Path
from typing import Any
from urllib.parse import unquote, urlsplit

from wasm_bundle_lock import generated_bundle_locked

MANIFEST_NAME = "pwmtf-bundle-manifest.json"
BUNDLE_HASH_ALGORITHM = "sha256-length-prefixed-v1"
BUNDLE_HASH_PATTERN = re.compile(r'^const candidateBundleHash = "([0-9a-f]{64})";$', re.MULTILINE)
SECURITY_HEADERS = {
    "Cache-Control": "no-store",
    "Content-Security-Policy": (
        "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; "
        "connect-src 'self'; img-src 'self'; font-src 'self'; media-src 'self'; "
        "worker-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'none'; "
        "frame-ancestors 'none'"
    ),
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Resource-Policy": "same-origin",
    "Permissions-Policy": "camera=(), geolocation=(), microphone=()",
    "Referrer-Policy": "same-origin",
    "X-Content-Type-Options": "nosniff",
}
CONTENT_TYPES = {
    ".css": "text/css; charset=utf-8",
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".svg": "image/svg+xml",
    ".wasm": "application/wasm",
    ".webmanifest": "application/manifest+json",
    ".png": "image/png",
}


def content_type(name: str) -> str:
    """Return the explicit response type for a generated asset."""
    if name.endswith(".d.ts"):
        return "text/plain; charset=utf-8"
    return CONTENT_TYPES.get(Path(name).suffix, "application/octet-stream")


def bundle_hash(assets: dict[str, bytes], names: list[str]) -> str:
    """Calculate the versioned length-prefixed identity of finalized assets."""
    digest = hashlib.sha256()
    for name in names:
        body = assets.get(name)
        if body is None:
            raise ValueError(f"bundle hash asset is missing: {name}")
        encoded_name = name.encode()
        digest.update(len(encoded_name).to_bytes(4, "big"))
        digest.update(encoded_name)
        digest.update(len(body).to_bytes(8, "big"))
        digest.update(body)
    return digest.hexdigest()


def load_assets(directory: Path) -> dict[str, bytes]:
    """Load exactly the manifest-listed assets before accepting requests."""
    manifest_path = directory / MANIFEST_NAME
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("bundle manifest must be a regular file")
    try:
        manifest: Any = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read bundle manifest: {error}") from error
    if not isinstance(manifest, dict) or not isinstance(manifest.get("assets"), dict):
        raise ValueError("bundle manifest assets are invalid")
    if manifest.get("bundle_hash_algorithm") != BUNDLE_HASH_ALGORITHM:
        raise ValueError("bundle hash algorithm is unsupported")
    hash_assets = manifest.get("bundle_hash_assets")
    if not isinstance(hash_assets, list) or any(not isinstance(name, str) for name in hash_assets):
        raise ValueError("bundle hash assets are invalid")

    assets: dict[str, bytes] = {}
    for name in manifest["assets"]:
        if not isinstance(name, str) or not name or "/" in name or "\\" in name:
            raise ValueError("bundle manifest contains an invalid asset name")
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"bundle asset must be a regular file: {name}")
        try:
            assets[name] = path.read_bytes()
        except OSError as error:
            raise ValueError(f"cannot read bundle asset {name}: {error}") from error
    if "index.html" not in assets:
        raise ValueError("bundle manifest does not contain index.html")
    bootstrap = assets.get("bootstrap.js")
    if bootstrap is None:
        raise ValueError("bundle manifest does not contain bootstrap.js")
    try:
        bundle_match = BUNDLE_HASH_PATTERN.search(bootstrap.decode("utf-8"))
    except UnicodeDecodeError as error:
        raise ValueError("bootstrap.js is not UTF-8") from error
    if bundle_match is None or bundle_match.group(1) != bundle_hash(assets, hash_assets):
        raise ValueError("candidate bundle hash does not match generated assets")
    for name, metadata in manifest["assets"].items():
        if not isinstance(metadata, dict) or set(metadata) != {"bytes", "sha256"}:
            raise ValueError(f"bundle manifest metadata is invalid: {name}")
        body = assets[name]
        if metadata["bytes"] != len(body) or metadata["sha256"] != hashlib.sha256(body).hexdigest():
            raise ValueError(f"bundle assets do not match integrity manifest: {name}")
    return assets


def handler_for(assets: dict[str, bytes]) -> type[http.server.BaseHTTPRequestHandler]:
    """Create a request handler bound to immutable in-memory bundle assets."""

    class BundleHandler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def end_headers(self) -> None:
            for name, value in SECURITY_HEADERS.items():
                self.send_header(name, value)
            super().end_headers()

        def do_GET(self) -> None:  # noqa: N802
            self.serve_asset(send_body=True)

        def do_HEAD(self) -> None:  # noqa: N802
            self.serve_asset(send_body=False)

        def serve_asset(self, *, send_body: bool) -> None:
            request_path = unquote(urlsplit(self.path).path)
            name = "index.html" if request_path in ("", "/") else request_path.removeprefix("/")
            body = assets.get(name)
            if body is None or "/" in name or "\\" in name:
                self.send_response(404)
                self.send_header("Content-Type", "text/plain; charset=utf-8")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", content_type(name))
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if send_body:
                self.wfile.write(body)

        def log_message(self, format: str, *args: object) -> None:
            return

    return BundleHandler


class BundleServer(http.server.ThreadingHTTPServer):
    """Bounded threaded server for local compatibility and physical testing."""

    allow_reuse_address = False
    daemon_threads = True
    request_queue_size = 16

    def get_request(self) -> tuple[Any, Any]:
        request, address = super().get_request()
        request.settimeout(5)
        return request, address


@generated_bundle_locked
def main() -> int:
    """Load the complete bundle, configure optional TLS, and serve it."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", required=True, type=int)
    parser.add_argument("--directory", default="dist", type=Path)
    parser.add_argument("--certificate", type=Path)
    parser.add_argument("--private-key", type=Path)
    arguments = parser.parse_args()
    if not 1 <= arguments.port <= 65_535:
        parser.error("--port must be from 1 through 65535")
    if (arguments.certificate is None) != (arguments.private_key is None):
        parser.error("--certificate and --private-key must be provided together")

    try:
        assets = load_assets(arguments.directory.resolve())
    except ValueError as error:
        parser.error(str(error))
    try:
        server = BundleServer((arguments.bind, arguments.port), handler_for(assets))
    except OSError as error:
        parser.error(f"cannot bind bundle server: {error}")
    if arguments.certificate is not None and arguments.private_key is not None:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        try:
            context.load_cert_chain(arguments.certificate, arguments.private_key)
        except (OSError, ssl.SSLError) as error:
            server.server_close()
            parser.error(f"cannot load TLS certificate and key: {error}")
        server.socket = context.wrap_socket(server.socket, server_side=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
