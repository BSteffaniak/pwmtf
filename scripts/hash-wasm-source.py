#!/usr/bin/env python3
"""Print the deterministic source identity used by PWMTF browser builds."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path

DECLARED_FILES = (
    "Cargo.lock",
    "Cargo.toml",
    "flake.lock",
    "flake.nix",
    "rust-toolchain.toml",
    "scripts/build-wasm.sh",
    "scripts/hash-wasm-source.py",
    "scripts/report-wasm-size.sh",
    "scripts/serve-feasibility.sh",
    "scripts/serve-wasm-bundle.py",
    "scripts/summarize-feasibility.py",
    "scripts/validate-feasibility-matrix.sh",
    "scripts/verify-wasm-bundle.py",
    "scripts/wasm_bundle_lock.py",
    "scripts/with-wasm-bundle-lock.py",
    "scripts/write-wasm-size-evidence.py",
)
DECLARED_DIRECTORIES = (
    ".cargo",
    "packages/billiards",
    "packages/client",
    "packages/eight_ball",
    "packages/protocol",
    "packages/sync",
)


def source_inputs(root: Path) -> list[Path]:
    """Return the complete, validated browser-build and evidence input set."""
    inputs: list[Path] = []
    for relative in DECLARED_FILES:
        path = root / relative
        if path.is_symlink() or not path.is_file():
            raise SystemExit(f"declared PWMTF WASM source input is not a regular file: {relative}")
        inputs.append(path)
    for relative in DECLARED_DIRECTORIES:
        directory = root / relative
        if directory.is_symlink() or not directory.is_dir():
            raise SystemExit(f"declared PWMTF WASM source input is not a directory: {relative}")
        inputs.extend(
            path
            for path in directory.rglob("*")
            if path.is_file() and not path.is_symlink()
        )

    extra_inputs = os.environ.get("PWMTF_WASM_SOURCE_INPUTS", "")
    for value in extra_inputs.split(os.pathsep):
        if not value:
            continue
        path = Path(value)
        if not path.is_absolute():
            path = root / path
        if path.is_symlink() or not path.is_file():
            raise SystemExit(f"PWMTF_WASM_SOURCE_INPUTS is not a regular file: {value}")
        inputs.append(path)

    resolved_root = root.resolve()
    resolved_inputs = {path.resolve() for path in inputs}
    try:
        return sorted(
            resolved_inputs,
            key=lambda candidate: candidate.relative_to(resolved_root).as_posix(),
        )
    except ValueError as error:
        raise SystemExit("PWMTF WASM source inputs must be inside the repository") from error


def main() -> None:
    """Hash or list the complete declared browser-build input set."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list-inputs",
        action="store_true",
        help="print repository-relative declared inputs instead of their hash",
    )
    arguments = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    inputs = source_inputs(root)
    if arguments.list_inputs:
        for path in inputs:
            print(path.relative_to(root).as_posix())
        return

    digest = hashlib.sha256()
    for path in inputs:
        relative = path.relative_to(root).as_posix().encode()
        contents = path.read_bytes()
        digest.update(len(relative).to_bytes(4, "big"))
        digest.update(relative)
        digest.update(len(contents).to_bytes(8, "big"))
        digest.update(contents)
    print(digest.hexdigest())


if __name__ == "__main__":
    main()
