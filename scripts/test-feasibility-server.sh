#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if [ "${PWMTF_WASM_BUNDLE_LOCKED:-0}" != 1 ]; then
    exec "$root/scripts/with-wasm-bundle-lock.py" -- "$0" "$@"
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/pwmtf-feasibility-server-test.XXXXXX")
server_pid=
cleanup() {
    if [ -n "$server_pid" ]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

for tool in curl openssl python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf '%s\n' "feasibility server self-test requires $tool" >&2
        exit 1
    fi
done

if [ ! -f "$root/dist/bootstrap.js" ] || \
   ! grep -q '^const candidateWasmOptimization = "wasm-opt-Oz";$' "$root/dist/bootstrap.js"; then
    "$root/scripts/build-wasm.sh"
fi

mkdir -p "$root/dist/stale/nested"
printf '%s\n' stale >"$root/dist/stale/nested/artifact.txt"
PWMTF_SKIP_WASM_OPT=1 "$root/scripts/build-wasm.sh" >"$tmp/stale-build.log" 2>&1
if [ -e "$root/dist/stale" ]; then
    printf '%s\n' "WASM build retained a stale dist directory" >&2
    exit 1
fi
"$root/scripts/build-wasm.sh" >"$tmp/optimized-build.log" 2>&1

openssl req \
    -x509 \
    -newkey rsa:2048 \
    -keyout "$tmp/key.pem" \
    -out "$tmp/cert.pem" \
    -days 2 \
    -nodes \
    -subj '/CN=localhost' \
    -addext 'subjectAltName=DNS:localhost' \
    >"$tmp/openssl.log" 2>&1
openssl req \
    -x509 \
    -newkey rsa:2048 \
    -keyout "$tmp/short-key.pem" \
    -out "$tmp/short-cert.pem" \
    -days 1 \
    -nodes \
    -subj '/CN=localhost' \
    -addext 'subjectAltName=DNS:localhost' \
    >>"$tmp/openssl.log" 2>&1
openssl req \
    -x509 \
    -newkey rsa:2048 \
    -keyout "$tmp/ip-key.pem" \
    -out "$tmp/ip-cert.pem" \
    -days 2 \
    -nodes \
    -subj '/CN=127.0.0.1' \
    -addext 'subjectAltName=IP:127.0.0.1' \
    >>"$tmp/openssl.log" 2>&1
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$tmp/wrong-key.pem" \
    >>"$tmp/openssl.log" 2>&1

expect_rejected() {
    label=$1
    shift
    if "$@" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
        printf '%s\n' "feasibility server accepted $label" >&2
        exit 1
    fi
}

expect_rejected \
    "an invalid listener port" \
    env \
    PWMTF_SKIP_BUILD=1 \
    PWMTF_FEASIBILITY_HOST=localhost \
    PWMTF_TLS_CERT="$tmp/cert.pem" \
    PWMTF_TLS_KEY="$tmp/key.pem" \
    PWMTF_FEASIBILITY_BIND=127.0.0.1 \
    PWMTF_FEASIBILITY_PORT=70000 \
    "$root/scripts/serve-feasibility.sh"
grep -q 'must be an integer from 1 through 65535' "$tmp/rejected.err"

expect_rejected \
    "a listener port with trailing whitespace" \
    env \
    PWMTF_SKIP_BUILD=1 \
    PWMTF_FEASIBILITY_HOST=localhost \
    PWMTF_TLS_CERT="$tmp/cert.pem" \
    PWMTF_TLS_KEY="$tmp/key.pem" \
    PWMTF_FEASIBILITY_PORT='8443 ' \
    "$root/scripts/serve-feasibility.sh"
grep -q 'must be an integer from 1 through 65535' "$tmp/rejected.err"

expect_rejected \
    "a near-expiry certificate" \
    env \
    PWMTF_SKIP_BUILD=1 \
    PWMTF_FEASIBILITY_HOST=localhost \
    PWMTF_TLS_CERT="$tmp/short-cert.pem" \
    PWMTF_TLS_KEY="$tmp/short-key.pem" \
    "$root/scripts/serve-feasibility.sh"
grep -q 'expires within 24 hours' "$tmp/rejected.err"

expect_rejected \
    "a certificate for another host" \
    env \
    PWMTF_SKIP_BUILD=1 \
    PWMTF_FEASIBILITY_HOST=wrong.example \
    PWMTF_TLS_CERT="$tmp/cert.pem" \
    PWMTF_TLS_KEY="$tmp/key.pem" \
    "$root/scripts/serve-feasibility.sh"
grep -q 'certificate does not cover' "$tmp/rejected.err"

expect_rejected \
    "a mismatched private key" \
    env \
    PWMTF_SKIP_BUILD=1 \
    PWMTF_FEASIBILITY_HOST=localhost \
    PWMTF_TLS_CERT="$tmp/cert.pem" \
    PWMTF_TLS_KEY="$tmp/wrong-key.pem" \
    "$root/scripts/serve-feasibility.sh"
grep -q 'private key does not match' "$tmp/rejected.err"

ip_port=${PWMTF_FEASIBILITY_IP_TEST_PORT:-48445}
env \
    PWMTF_SKIP_BUILD=1 \
    PWMTF_FEASIBILITY_HOST=127.0.0.1 \
    PWMTF_TLS_CERT="$tmp/ip-cert.pem" \
    PWMTF_TLS_KEY="$tmp/ip-key.pem" \
    PWMTF_FEASIBILITY_BIND=127.0.0.1 \
    PWMTF_FEASIBILITY_PORT="$ip_port" \
    "$root/scripts/serve-feasibility.sh" >"$tmp/ip-server.log" 2>&1 &
server_pid=$!
sleep 0.5
if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$tmp/ip-server.log" >&2
    printf '%s\n' "IP SAN feasibility server did not start" >&2
    exit 1
fi
kill "$server_pid"
wait "$server_pid" 2>/dev/null || true
server_pid=

port=${PWMTF_FEASIBILITY_TEST_PORT:-48444}
env \
    PWMTF_SKIP_BUILD=1 \
    PWMTF_FEASIBILITY_HOST=localhost \
    PWMTF_TLS_CERT="$tmp/cert.pem" \
    PWMTF_TLS_KEY="$tmp/key.pem" \
    PWMTF_FEASIBILITY_BIND=127.0.0.1 \
    PWMTF_FEASIBILITY_PORT="$port" \
    "$root/scripts/serve-feasibility.sh" >"$tmp/server.log" 2>&1 &
server_pid=$!

attempt=0
while ! curl --insecure --fail --silent --output /dev/null "https://127.0.0.1:$port/"; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 50 ]; then
        cat "$tmp/server.log" >&2
        printf '%s\n' "local feasibility HTTPS server did not start" >&2
        exit 1
    fi
    sleep 0.1
done

curl \
    --insecure \
    --fail \
    --silent \
    --dump-header "$tmp/headers" \
    --output /dev/null \
    "https://127.0.0.1:$port/?feasibility"
grep -qi '^Cache-Control: no-store' "$tmp/headers"
grep -qi '^Permissions-Policy: camera=(), geolocation=(), microphone=()' "$tmp/headers"
grep -qi '^Referrer-Policy: same-origin' "$tmp/headers"
grep -qi '^X-Content-Type-Options: nosniff' "$tmp/headers"
grep -qi '^Cross-Origin-Opener-Policy: same-origin' "$tmp/headers"
grep -qi '^Cross-Origin-Resource-Policy: same-origin' "$tmp/headers"
grep -qi "^Content-Security-Policy: default-src 'none';" "$tmp/headers"
grep -qi '^Content-Type: text/html; charset=utf-8' "$tmp/headers"

curl \
    --insecure \
    --fail \
    --silent \
    --dump-header "$tmp/asset-headers" \
    --output "$tmp/served-bootstrap.js" \
    "https://127.0.0.1:$port/bootstrap.js"
cmp "$root/dist/bootstrap.js" "$tmp/served-bootstrap.js"
grep -qi '^Content-Type: text/javascript; charset=utf-8' "$tmp/asset-headers"

curl \
    --insecure \
    --fail \
    --silent \
    --dump-header "$tmp/feasibility-headers" \
    --output "$tmp/served-feasibility.js" \
    "https://127.0.0.1:$port/feasibility.js"
cmp "$root/dist/feasibility.js" "$tmp/served-feasibility.js"
grep -qi '^Content-Type: text/javascript; charset=utf-8' "$tmp/feasibility-headers"

curl \
    --insecure \
    --fail \
    --silent \
    --head \
    "https://127.0.0.1:$port/pwmtf_client_bg.wasm" >"$tmp/wasm-headers"
grep -qi '^Content-Type: application/wasm' "$tmp/wasm-headers"
grep -qi '^Content-Length: [1-9][0-9]*' "$tmp/wasm-headers"

for path in pwmtf-bundle-manifest.json %2e%2e%2fCargo.toml missing.txt; do
    status=$(curl --insecure --silent --output /dev/null --write-out '%{http_code}' \
        "https://127.0.0.1:$port/$path")
    if [ "$status" != 404 ]; then
        printf '%s\n' "feasibility server exposed non-asset path $path with status $status" >&2
        exit 1
    fi
done

printf '%s\n' "feasibility HTTPS server self-tests passed"
