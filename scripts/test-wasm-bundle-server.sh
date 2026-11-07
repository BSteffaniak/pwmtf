#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if [ "${PWMTF_WASM_BUNDLE_LOCKED:-0}" != 1 ]; then
    exec "$root/scripts/with-wasm-bundle-lock.py" -- "$0" "$@"
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/pwmtf-wasm-server-test.XXXXXX")
server_pid=
cleanup() {
    if [ -n "$server_pid" ]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

if [ ! -f "$root/dist/pwmtf-bundle-manifest.json" ]; then
    PWMTF_SKIP_WASM_OPT=1 "$root/scripts/build-wasm.sh"
fi

expect_rejected() {
    label=$1
    shift
    if "$@" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
        printf '%s\n' "WASM bundle server accepted $label" >&2
        exit 1
    fi
}

expect_rejected "an out-of-range port" \
    "$root/scripts/serve-wasm-bundle.py" --port 70000 --directory "$root/dist"
grep -q -- '--port must be from 1 through 65535' "$tmp/rejected.err"

expect_rejected "an incomplete TLS configuration" \
    "$root/scripts/serve-wasm-bundle.py" --port 4189 --directory "$root/dist" --certificate missing.pem
grep -q -- '--certificate and --private-key must be provided together' "$tmp/rejected.err"

cp -R "$root/dist" "$tmp/tampered"
printf '%s\n' tampered >>"$tmp/tampered/styles.css"
expect_rejected "a tampered bundle" \
    "$root/scripts/serve-wasm-bundle.py" --port 4189 --directory "$tmp/tampered"
grep -q 'candidate bundle hash does not match generated assets\|bundle assets do not match' "$tmp/rejected.err"

port=${PWMTF_WASM_SERVER_TEST_PORT:-}
if [ -z "$port" ]; then
    port=$(python3 - <<'PY'
import socket

with socket.socket() as listener:
    listener.bind(("127.0.0.1", 0))
    print(listener.getsockname()[1])
PY
)
fi
"$root/scripts/serve-wasm-bundle.py" \
    --bind 127.0.0.1 \
    --port "$port" \
    --directory "$root/dist" >"$tmp/server.log" 2>&1 &
server_pid=$!
attempt=0
while ! curl --fail --silent --output /dev/null "http://127.0.0.1:$port/"; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 50 ]; then
        cat "$tmp/server.log" >&2
        printf '%s\n' "WASM bundle server did not start" >&2
        exit 1
    fi
    sleep 0.1
done

curl --fail --silent --dump-header "$tmp/headers" --output "$tmp/bootstrap.js" \
    "http://127.0.0.1:$port/bootstrap.js"
cmp "$root/dist/bootstrap.js" "$tmp/bootstrap.js"
grep -qi '^Content-Type: text/javascript; charset=utf-8' "$tmp/headers"
grep -qi '^Content-Security-Policy: default-src' "$tmp/headers"
grep -qi '^Cross-Origin-Opener-Policy: same-origin' "$tmp/headers"
grep -qi '^Cross-Origin-Resource-Policy: same-origin' "$tmp/headers"

curl --fail --silent --dump-header "$tmp/feasibility-headers" --output "$tmp/feasibility.js" \
    "http://127.0.0.1:$port/feasibility.js"
cmp "$root/dist/feasibility.js" "$tmp/feasibility.js"
grep -qi '^Content-Type: text/javascript; charset=utf-8' "$tmp/feasibility-headers"

curl --fail --silent --dump-header "$tmp/brand-headers" --output "$tmp/brand-mark.svg" \
    "http://127.0.0.1:$port/brand-mark.svg"
cmp "$root/dist/brand-mark.svg" "$tmp/brand-mark.svg"
grep -qi '^Content-Type: image/svg+xml' "$tmp/brand-headers"

for method in GET HEAD; do
    if [ "$method" = HEAD ]; then
        curl --silent --head "http://127.0.0.1:$port/pwmtf-bundle-manifest.json" >"$tmp/missing-headers"
    else
        curl --silent --dump-header "$tmp/missing-headers" --output "$tmp/missing-body" \
            "http://127.0.0.1:$port/pwmtf-bundle-manifest.json"
        if [ -s "$tmp/missing-body" ]; then
            printf '%s\n' "WASM bundle server returned a body for a missing asset" >&2
            exit 1
        fi
    fi
    status=$(awk 'NR == 1 { print $2 }' "$tmp/missing-headers")
    grep -qi '^Content-Length: 0' "$tmp/missing-headers"
    if [ "$status" != 404 ]; then
        printf '%s\n' "WASM bundle server exposed manifest via $method with status $status" >&2
        exit 1
    fi
done

printf '%s\n' "WASM bundle server self-tests passed"
