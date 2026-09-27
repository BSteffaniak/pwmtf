#!/bin/sh
# Two isolated real browser sessions against a locally seeded authoritative match.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
if [ "${PWMTF_WASM_BUNDLE_LOCKED:-0}" != 1 ]; then
    exec ./scripts/with-wasm-bundle-lock.py -- "$0" "$@"
fi
PWMTF_SKIP_WASM_OPT=1 ./scripts/build-wasm.sh
cargo build --package pwmtf_server --features insecure --bin pwmtf-server --example seed_native_smoke
chrome=${CHROME_BIN:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}
[ -x "$chrome" ] || { echo 'Chrome unavailable for two-browser check' >&2; exit 1; }
tmp=$(mktemp -d "${TMPDIR:-/tmp}/pwmtf-two-browser.XXXXXX")
cleanup() {
    if [ -n "${pid:-}" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
    rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM
port=${PWMTF_TWO_BROWSER_PORT:-4194}
target/debug/examples/seed_native_smoke "$tmp/match.db" > "$tmp/seed.out"
PWMTF_DEV_MODE=true PWMTF_CANONICAL_ORIGIN="http://127.0.0.1:$port" PWMTF_BIND="127.0.0.1:$port" PWMTF_DATABASE_PATH="$tmp/match.db" PWMTF_WEB_ROOT="$root/dist" target/debug/pwmtf-server > "$tmp/server.log" 2>&1 &
pid=$!
tries=0
until curl --fail --silent --output /dev/null "http://127.0.0.1:$port/readyz"; do
    tries=$((tries + 1))
    if [ "$tries" -ge 100 ] || ! kill -0 "$pid" 2>/dev/null; then cat "$tmp/server.log" >&2; exit 1; fi
    sleep 0.1
done
node ./scripts/test-two-browser-shot.js "$chrome" "$port" "$tmp/seed.out"
