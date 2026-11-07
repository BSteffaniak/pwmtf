#!/usr/bin/env python3
"""Self-test deterministic PWMTF browser-build source hashing."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile

OUTSIDE_INPUT = Path("/etc/hosts")


def source_hash(root: Path, extra: str | None = None) -> str:
    """Run the source hasher with an optional extra declared input."""
    environment = os.environ.copy()
    if extra is None:
        environment.pop("PWMTF_WASM_SOURCE_INPUTS", None)
    else:
        environment["PWMTF_WASM_SOURCE_INPUTS"] = extra
    return subprocess.check_output(
        [str(root / "scripts" / "hash-wasm-source.py")],
        cwd=root,
        env=environment,
        text=True,
    ).strip()


def main() -> None:
    """Prove stable hashing and sensitivity to every declared input class."""
    root = Path(__file__).resolve().parent.parent
    first = source_hash(root)
    second = source_hash(root)
    if first != second or len(first) != 64:
        raise SystemExit("browser source hash is not stable SHA-256 output")

    listed = subprocess.check_output(
        [str(root / "scripts" / "hash-wasm-source.py"), "--list-inputs"],
        cwd=root,
        text=True,
    ).splitlines()
    required = {
        "Cargo.lock",
        "Cargo.toml",
        "flake.lock",
        "flake.nix",
        "rust-toolchain.toml",
        "packages/client/src/main.rs",
        "packages/client/web/bootstrap.js",
        "packages/client/web/feasibility.js",
        "scripts/build-wasm.sh",
        "scripts/serve-wasm-bundle.py",
        "scripts/summarize-feasibility.py",
        "scripts/validate-feasibility-matrix.sh",
        "scripts/verify-wasm-bundle.py",
        "scripts/wasm_bundle_lock.py",
        "scripts/write-wasm-size-evidence.py",
    }
    missing_declared = required - set(listed)
    if missing_declared:
        raise SystemExit(f"browser source hash omitted declared inputs: {sorted(missing_declared)}")
    if listed != sorted(set(listed)):
        raise SystemExit("browser source hash input listing is not sorted and unique")

    with tempfile.TemporaryDirectory(dir=root, prefix=".pwmtf-source-hash-test-") as directory:
        extra = Path(directory) / "extra-input"
        extra.write_text("first\n", encoding="utf-8")
        relative = extra.relative_to(root).as_posix()
        before = source_hash(root, relative)
        extra.write_text("second\n", encoding="utf-8")
        after = source_hash(root, relative)
        if before == after:
            raise SystemExit("browser source hash ignored a changed declared input")

        missing = subprocess.run(
            [str(root / "scripts" / "hash-wasm-source.py")],
            cwd=root,
            env={**os.environ, "PWMTF_WASM_SOURCE_INPUTS": f"{relative}-missing"},
            capture_output=True,
            text=True,
            check=False,
        )
        if missing.returncode == 0 or "is not a regular file" not in missing.stderr:
            raise SystemExit("browser source hash accepted a missing declared input")

        symlink = Path(directory) / "symlink-input"
        symlink.symlink_to(extra)
        linked = subprocess.run(
            [str(root / "scripts" / "hash-wasm-source.py")],
            cwd=root,
            env={
                **os.environ,
                "PWMTF_WASM_SOURCE_INPUTS": symlink.relative_to(root).as_posix(),
            },
            capture_output=True,
            text=True,
            check=False,
        )
        if linked.returncode == 0 or "is not a regular file" not in linked.stderr:
            raise SystemExit("browser source hash accepted a symlinked declared input")

        outside = subprocess.run(
            [str(root / "scripts" / "hash-wasm-source.py")],
            cwd=root,
            env={**os.environ, "PWMTF_WASM_SOURCE_INPUTS": str(OUTSIDE_INPUT)},
            capture_output=True,
            text=True,
            check=False,
        )
        if outside.returncode == 0 or "must be inside the repository" not in outside.stderr:
            raise SystemExit("browser source hash accepted an outside declared input")

    print("WASM source-hash self-tests passed")


if __name__ == "__main__":
    main()
