#!/usr/bin/env python3
"""Write or verify the integrity manifest for the generated PWMTF WASM bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

from wasm_bundle_lock import generated_bundle_locked

MANIFEST_NAME = "pwmtf-bundle-manifest.json"
BUNDLE_HASH_ALGORITHM = "sha256-length-prefixed-v1"
EXPECTED_ASSETS = {
    "bootstrap.js",
    "brand-mark.svg",
    "feasibility.js",
    "manifest.webmanifest",
    "install.js",
    "icon-180.png",
    "icon-192.png",
    "icon-512.png",
    "index.html",
    "pwmtf_client.d.ts",
    "pwmtf_client.js",
    "pwmtf_client_bg.wasm",
    "pwmtf_client_bg.wasm.d.ts",
    "styles.css",
}
BUNDLE_HASH_ASSETS = EXPECTED_ASSETS - {"bootstrap.js"}
CANDIDATE_PATTERNS = {
    "build_id": re.compile(r'^const candidateBuildId = "([A-Za-z0-9._-]+)";$', re.MULTILINE),
    "source_hash": re.compile(r'^const candidateSourceHash = "([0-9a-f]{64})";$', re.MULTILINE),
    "bundle_hash": re.compile(r'^const candidateBundleHash = "([0-9a-f]{64})";$', re.MULTILINE),
    "wasm_optimization": re.compile(
        r'^const candidateWasmOptimization = "(wasm-opt-Oz|not-applied)";$', re.MULTILINE
    ),
}


def candidate_identity(directory: Path) -> dict[str, str]:
    """Read the injected candidate identity from bootstrap.js."""
    try:
        contents = (directory / "bootstrap.js").read_text(encoding="utf-8")
    except OSError as error:
        raise ValueError(f"cannot read bootstrap.js: {error}") from error

    candidate: dict[str, str] = {}
    for field, pattern in CANDIDATE_PATTERNS.items():
        match = pattern.search(contents)
        if match is None:
            raise ValueError(f"bootstrap.js does not contain a valid candidate {field}")
        candidate[field] = match.group(1)
    if candidate["source_hash"] not in candidate["build_id"]:
        raise ValueError("candidate build ID does not include the candidate source hash")
    return candidate


def calculated_bundle_hash(directory: Path) -> str:
    """Calculate the pre-bootstrap hash injected into feasibility reports."""
    digest = hashlib.sha256()
    for name in sorted(BUNDLE_HASH_ASSETS):
        path = directory / name
        encoded_name = name.encode()
        try:
            body = path.read_bytes()
        except OSError as error:
            raise ValueError(f"cannot read {name}: {error}") from error
        digest.update(len(encoded_name).to_bytes(4, "big"))
        digest.update(encoded_name)
        digest.update(len(body).to_bytes(8, "big"))
        digest.update(body)
    return digest.hexdigest()


def asset_metadata(path: Path) -> dict[str, Any]:
    """Return stable integrity metadata for one generated asset."""
    digest = hashlib.sha256()
    size = 0
    try:
        with path.open("rb") as asset:
            while chunk := asset.read(1024 * 1024):
                digest.update(chunk)
                size += len(chunk)
    except OSError as error:
        raise ValueError(f"cannot read {path.name}: {error}") from error
    return {"bytes": size, "sha256": digest.hexdigest()}


def bundle_assets(directory: Path) -> dict[str, dict[str, Any]]:
    """Collect the exact regular-file asset set for a generated bundle."""
    try:
        entries = list(directory.iterdir())
    except OSError as error:
        raise ValueError(f"cannot read bundle directory: {error}") from error

    names: set[str] = set()
    for entry in entries:
        if entry.name == MANIFEST_NAME:
            continue
        if entry.is_symlink() or not entry.is_file():
            raise ValueError(f"bundle contains a non-regular asset: {entry.name}")
        names.add(entry.name)
    if names != EXPECTED_ASSETS:
        missing = sorted(EXPECTED_ASSETS - names)
        unexpected = sorted(names - EXPECTED_ASSETS)
        details = []
        if missing:
            details.append(f"missing {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected {', '.join(unexpected)}")
        raise ValueError(f"bundle asset set is invalid: {'; '.join(details)}")
    return {name: asset_metadata(directory / name) for name in sorted(names)}


def write_manifest(directory: Path) -> None:
    """Create a manifest for the complete generated bundle."""
    manifest = {
        "schema_version": 1,
        "bundle_hash_algorithm": BUNDLE_HASH_ALGORITHM,
        "bundle_hash_assets": sorted(BUNDLE_HASH_ASSETS),
        "candidate": candidate_identity(directory),
        "assets": bundle_assets(directory),
    }
    (directory / MANIFEST_NAME).write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def load_manifest(directory: Path) -> dict[str, Any]:
    """Load the checked-in bundle manifest representation."""
    manifest_path = directory / MANIFEST_NAME
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("bundle manifest must be a regular file")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {MANIFEST_NAME}: {error}") from error
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema_version",
        "bundle_hash_algorithm",
        "bundle_hash_assets",
        "candidate",
        "assets",
    }:
        raise ValueError("bundle manifest structure is invalid")
    if manifest["schema_version"] != 1:
        raise ValueError("bundle manifest schema version is unsupported")
    if manifest["bundle_hash_algorithm"] != BUNDLE_HASH_ALGORITHM:
        raise ValueError("bundle hash algorithm is unsupported")
    if manifest["bundle_hash_assets"] != sorted(BUNDLE_HASH_ASSETS):
        raise ValueError("bundle hash asset set is unsupported")
    if not isinstance(manifest["candidate"], dict) or not isinstance(manifest["assets"], dict):
        raise ValueError("bundle manifest candidate and assets must be objects")
    return manifest


def verify_manifest(directory: Path) -> None:
    """Verify candidate identity, bundle identity, asset set, sizes, and hashes."""
    manifest = load_manifest(directory)
    candidate = candidate_identity(directory)
    if manifest["candidate"] != candidate:
        raise ValueError("bundle manifest candidate identity does not match bootstrap.js")
    assets = bundle_assets(directory)
    if candidate["bundle_hash"] != calculated_bundle_hash(directory):
        raise ValueError("candidate bundle hash does not match generated assets")
    if manifest["assets"] != assets:
        raise ValueError("bundle assets do not match the integrity manifest")


@generated_bundle_locked
def main() -> int:
    """Run the manifest writer or verifier."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="write a new manifest instead of verifying")
    parser.add_argument("directory", nargs="?", default="dist", type=Path)
    arguments = parser.parse_args()
    try:
        if arguments.write:
            write_manifest(arguments.directory)
        else:
            verify_manifest(arguments.directory)
    except ValueError as error:
        print(f"WASM bundle integrity error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
