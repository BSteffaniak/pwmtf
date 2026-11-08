#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

if [ "${PWMTF_WASM_BUNDLE_LOCKED:-0}" != 1 ]; then
    exec ./scripts/with-wasm-bundle-lock.py -- "$0" "$@"
fi

source_hash=$(./scripts/hash-wasm-source.py)

build_id=${PWMTF_BUILD_ID:-}
if [ -z "$build_id" ]; then
    revision=$(git rev-parse --short=12 HEAD 2>/dev/null || printf '%s' unknown)
    build_id="$revision-$source_hash"
fi
case "$build_id" in
    *[!A-Za-z0-9._-]*|'')
        printf '%s\n' "PWMTF_BUILD_ID must contain only ASCII letters, digits, dot, underscore, or hyphen" >&2
        exit 1
        ;;
esac

cargo build --package pwmtf_client --target wasm32-unknown-unknown --release
python3 - <<'PY'
from pathlib import Path
import shutil

output = Path("dist")
output.mkdir(exist_ok=True)
for entry in output.iterdir():
    if entry.is_dir() and not entry.is_symlink():
        shutil.rmtree(entry)
    else:
        entry.unlink()
PY
cp packages/client/web/index.html packages/client/web/styles.css packages/client/web/bootstrap.js \
    packages/client/web/feasibility.js packages/client/web/brand-mark.svg packages/client/web/manifest.webmanifest \
    packages/client/web/install.js packages/client/web/icon-180.png \
    packages/client/web/icon-192.png packages/client/web/icon-512.png dist/
wasm_optimization=not-applied
if command -v wasm-opt >/dev/null 2>&1 && [ "${PWMTF_SKIP_WASM_OPT:-0}" != 1 ]; then
    wasm_optimization=wasm-opt-Oz
fi
wasm_schema=$(grep -m1 'name = "wasm-bindgen"' -A2 Cargo.lock | grep 'version = ' | cut -d'"' -f2)
cli_schema=$(wasm-bindgen --version | awk '{print $2}')
if [ "$wasm_schema" != "$cli_schema" ]; then
    printf '%s\n' "wasm-bindgen CLI $cli_schema does not match locked crate $wasm_schema" >&2
    printf '%s\n' "install wasm-bindgen-cli $wasm_schema, then rerun this script" >&2
    exit 1
fi
wasm-bindgen \
    --target web \
    --out-dir dist \
    --out-name pwmtf_client \
    target/wasm32-unknown-unknown/release/pwmtf-client.wasm

if command -v wasm-opt >/dev/null 2>&1; then
    optimized="$root/dist/pwmtf_client_bg.optimized.wasm"
    if [ "${PWMTF_SKIP_WASM_OPT:-0}" = 1 ]; then
        printf '%s\n' "PWMTF_SKIP_WASM_OPT=1: leaving release WASM unoptimized by Binaryen" >&2
    else
        wasm-opt \
            -Oz \
            --enable-bulk-memory \
            --enable-multivalue \
            --enable-mutable-globals \
            --enable-nontrapping-float-to-int \
            --enable-reference-types \
            --enable-sign-ext \
            "$root/dist/pwmtf_client_bg.wasm" \
            -o "$optimized"
        mv "$optimized" "$root/dist/pwmtf_client_bg.wasm"
    fi
fi

BUILD_ID="$build_id" SOURCE_HASH="$source_hash" WASM_OPTIMIZATION="$wasm_optimization" python3 - <<'PY'
import hashlib
import os
from pathlib import Path

path = Path("dist/bootstrap.js")
contents = path.read_text(encoding="utf-8")

# Version every asset reference with the build ID. The edge may cache CSS,
# JS and WASM for hours even though the origin says no-cache, so a new
# no-cache index.html must never load a previous deploy's assets.
version = os.environ["BUILD_ID"]


def stamp(text, references, label):
    for old, new in references:
        if text.count(old) != 1:
            raise SystemExit(f"{label} reference must occur exactly once: {old}")
        text = text.replace(old, new)
    return text


index_path = Path("dist/index.html")
index_path.write_text(
    stamp(
        index_path.read_text(encoding="utf-8"),
        (
            ('src="/install.js"', f'src="/install.js?v={version}"'),
            ('href="./styles.css"', f'href="./styles.css?v={version}"'),
            ('src="./bootstrap.js"', f'src="./bootstrap.js?v={version}"'),
        ),
        "index.html",
    ),
    encoding="utf-8",
)
contents = stamp(
    contents,
    (
        ('import("./feasibility.js")', f'import("./feasibility.js?v={version}")'),
        ('import("./pwmtf_client.js")', f'import("./pwmtf_client.js?v={version}")'),
        (
            "module.default()",
            f'module.default({{ module_or_path: "./pwmtf_client_bg.wasm?v={version}" }})',
        ),
    ),
    "bootstrap.js",
)
bundle_hash = hashlib.sha256()
assets = sorted(
    (
        "brand-mark.svg",
        "feasibility.js",
        "manifest.webmanifest",
        "install.js",
        "icon-180.png",
        "icon-192.png",
        "icon-512.png",
        "index.html",
        "styles.css",
        "pwmtf_client.d.ts",
        "pwmtf_client.js",
        "pwmtf_client_bg.wasm",
        "pwmtf_client_bg.wasm.d.ts",
    )
)
for asset in assets:
    name = asset.encode()
    body = Path("dist", asset).read_bytes()
    bundle_hash.update(len(name).to_bytes(4, "big"))
    bundle_hash.update(name)
    bundle_hash.update(len(body).to_bytes(8, "big"))
    bundle_hash.update(body)
replacements = {
    "__PWMTF_BUILD_ID__": os.environ["BUILD_ID"],
    "__PWMTF_SOURCE_HASH__": os.environ["SOURCE_HASH"],
    "__PWMTF_BUNDLE_HASH__": bundle_hash.hexdigest(),
    "__PWMTF_WASM_OPTIMIZATION__": os.environ["WASM_OPTIMIZATION"],
}
for placeholder, value in replacements.items():
    if contents.count(placeholder) != 1:
        raise SystemExit(f"bootstrap placeholder must occur exactly once: {placeholder}")
    contents = contents.replace(placeholder, value)
path.write_text(contents, encoding="utf-8")
PY

./scripts/verify-wasm-bundle.py --write dist
./scripts/verify-wasm-bundle.py dist

printf '%s\n' "PWMTF web client built in $root/dist (build $build_id)"
