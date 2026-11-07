#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if [ "${PWMTF_WASM_BUNDLE_LOCKED:-0}" != 1 ]; then
    exec "$root/scripts/with-wasm-bundle-lock.py" -- "$0" "$@"
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/pwmtf-bundle-integrity-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

if [ ! -f "$root/dist/pwmtf-bundle-manifest.json" ]; then
    "$root/scripts/build-wasm.sh"
fi

copy_bundle() {
    destination=$1
    mkdir -p "$destination"
    cp -R "$root/dist/." "$destination/"
}

expect_rejected() {
    label=$1
    directory=$2
    if "$root/scripts/verify-wasm-bundle.py" "$directory" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
        printf '%s\n' "WASM bundle verifier accepted $label" >&2
        exit 1
    fi
}

"$root/scripts/verify-wasm-bundle.py" "$root/dist"

copy_bundle "$tmp/tampered"
printf '%s\n' tampered >>"$tmp/tampered/styles.css"
expect_rejected "a modified asset" "$tmp/tampered"
grep -Eq 'candidate bundle hash does not match generated assets|do not match the integrity manifest' "$tmp/rejected.err"

copy_bundle "$tmp/tampered-feasibility"
printf '%s\n' '// tampered' >>"$tmp/tampered-feasibility/feasibility.js"
expect_rejected "a modified feasibility module" "$tmp/tampered-feasibility"
grep -Eq 'candidate bundle hash does not match generated assets|do not match the integrity manifest' "$tmp/rejected.err"

copy_bundle "$tmp/missing-feasibility"
rm "$tmp/missing-feasibility/feasibility.js"
expect_rejected "a missing feasibility module" "$tmp/missing-feasibility"
grep -q 'missing feasibility.js' "$tmp/rejected.err"

copy_bundle "$tmp/missing"
rm "$tmp/missing/index.html"
expect_rejected "a missing asset" "$tmp/missing"
grep -q 'missing index.html' "$tmp/rejected.err"

copy_bundle "$tmp/unexpected"
printf '%s\n' stale >"$tmp/unexpected/stale.txt"
expect_rejected "an unexpected asset" "$tmp/unexpected"
grep -q 'unexpected stale.txt' "$tmp/rejected.err"

copy_bundle "$tmp/identity"
python3 - "$tmp/identity/pwmtf-bundle-manifest.json" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
manifest["candidate"]["build_id"] = "different-build-" + manifest["candidate"]["source_hash"]
path.write_text(json.dumps(manifest), encoding="utf-8")
PY
expect_rejected "a mismatched candidate identity" "$tmp/identity"
grep -q 'candidate identity does not match' "$tmp/rejected.err"

copy_bundle "$tmp/bundle-identity"
python3 - "$tmp/bundle-identity/bootstrap.js" "$tmp/bundle-identity/pwmtf-bundle-manifest.json" <<'PY'
import hashlib
import json
import re
import sys
from pathlib import Path

bootstrap_path = Path(sys.argv[1])
manifest_path = Path(sys.argv[2])
bootstrap = bootstrap_path.read_text(encoding="utf-8")
wrong_hash = "f" * 64
bootstrap = re.sub(
    r'^const candidateBundleHash = "[0-9a-f]{64}";$',
    f'const candidateBundleHash = "{wrong_hash}";',
    bootstrap,
    flags=re.MULTILINE,
)
bootstrap_path.write_text(bootstrap, encoding="utf-8")
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
manifest["candidate"]["bundle_hash"] = wrong_hash
body = bootstrap_path.read_bytes()
manifest["assets"]["bootstrap.js"] = {
    "bytes": len(body),
    "sha256": hashlib.sha256(body).hexdigest(),
}
manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
PY
expect_rejected "a bundle identity not derived from generated assets" "$tmp/bundle-identity"
grep -q 'candidate bundle hash does not match generated assets' "$tmp/rejected.err"

copy_bundle "$tmp/symlink"
rm "$tmp/symlink/styles.css"
ln -s "$root/dist/styles.css" "$tmp/symlink/styles.css"
expect_rejected "a symlinked asset" "$tmp/symlink"
grep -q 'non-regular asset: styles.css' "$tmp/rejected.err"

copy_bundle "$tmp/manifest-algorithm"
python3 - "$tmp/manifest-algorithm/pwmtf-bundle-manifest.json" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
manifest["bundle_hash_algorithm"] = "unknown"
path.write_text(json.dumps(manifest), encoding="utf-8")
PY
expect_rejected "an unknown bundle hash algorithm" "$tmp/manifest-algorithm"
grep -q 'bundle hash algorithm is unsupported' "$tmp/rejected.err"

copy_bundle "$tmp/manifest-assets"
python3 - "$tmp/manifest-assets/pwmtf-bundle-manifest.json" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
manifest["bundle_hash_assets"] = ["styles.css"]
path.write_text(json.dumps(manifest), encoding="utf-8")
PY
expect_rejected "an unknown bundle hash asset set" "$tmp/manifest-assets"
grep -q 'bundle hash asset set is unsupported' "$tmp/rejected.err"

copy_bundle "$tmp/manifest-symlink"
rm "$tmp/manifest-symlink/pwmtf-bundle-manifest.json"
ln -s "$root/dist/pwmtf-bundle-manifest.json" "$tmp/manifest-symlink/pwmtf-bundle-manifest.json"
expect_rejected "a symlinked manifest" "$tmp/manifest-symlink"
grep -q 'bundle manifest must be a regular file' "$tmp/rejected.err"

printf '%s\n' "WASM bundle integrity self-tests passed"
