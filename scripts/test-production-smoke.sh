#!/bin/sh
set -eu

canonical_origin=${PWMTF_CANONICAL_ORIGIN:-https://pwmtf.hyperchad.dev}
directory_url=${PWMTF_DIRECTORY_URL:-https://hyperchad.dev/games/pool-with-more-than-friends}
expected_directory_target=${PWMTF_DIRECTORY_TARGET:-$canonical_origin}
max_time=${PWMTF_PRODUCTION_SMOKE_TIMEOUT_SECONDS:-20}
curl_bin=${PWMTF_CURL_BIN:-curl}
mode=${1:-all}
if [ "$#" -gt 1 ] || { [ "$mode" != all ] && [ "$mode" != --origin-only ]; }; then
    printf '%s\n' "usage: $0 [--origin-only]" >&2
    exit 2
fi

if ! command -v "$curl_bin" >/dev/null 2>&1; then
    printf '%s\n' "PWMTF_CURL_BIN must name an executable curl-compatible command" >&2
    exit 1
fi

case "$canonical_origin" in
    https://pwmtf.hyperchad.dev|https://pwmtf.hyperchad.dev/)
        ;;
    *)
        printf '%s\n' "PWMTF_CANONICAL_ORIGIN must be the canonical production origin" >&2
        exit 1
        ;;
esac
canonical_origin=${canonical_origin%/}
expected_directory_target=${expected_directory_target%/}

case "$max_time" in
    ''|*[!0-9]*)
        printf '%s\n' "PWMTF_PRODUCTION_SMOKE_TIMEOUT_SECONDS must be a positive integer" >&2
        exit 1
        ;;
esac
if [ "$max_time" -lt 1 ] || [ "$max_time" -gt 120 ]; then
    printf '%s\n' "PWMTF_PRODUCTION_SMOKE_TIMEOUT_SECONDS must be from 1 through 120" >&2
    exit 1
fi

response_headers=$(mktemp "${TMPDIR:-/tmp}/pwmtf-production-smoke.XXXXXX")
response_body=$(mktemp "${TMPDIR:-/tmp}/pwmtf-production-smoke-body.XXXXXX")
trap 'rm -f "$response_headers" "$response_body"' EXIT HUP INT TERM

request() {
    url=$1
    "$curl_bin" \
        --fail \
        --silent \
        --show-error \
        --connect-timeout "$max_time" \
        --max-time "$max_time" \
        --dump-header "$response_headers" \
        --output "$response_body" \
        "$url"
}

request "$canonical_origin/healthz"
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
if [ "$status" != 200 ] || [ "$(cat "$response_body")" != ok ]; then
    printf '%s\n' "canonical health check did not return exact 200/ok" >&2
    exit 1
fi
grep -qi '^strict-transport-security: max-age=63072000; includeSubDomains' "$response_headers"
grep -qi '^cache-control: no-store' "$response_headers"
grep -qi '^referrer-policy: same-origin' "$response_headers"
grep -qi '^x-content-type-options: nosniff' "$response_headers"
grep -qi '^permissions-policy: camera=(), geolocation=(), microphone=()' "$response_headers"
grep -qi '^cross-origin-opener-policy: same-origin' "$response_headers"
grep -qi '^cross-origin-resource-policy: same-origin' "$response_headers"

request "$canonical_origin/readyz"
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
if [ "$status" != 200 ] || [ "$(cat "$response_body")" != ready ]; then
    printf '%s\n' "canonical readiness check did not return exact 200/ready" >&2
    exit 1
fi
grep -qi '^strict-transport-security:' "$response_headers"
grep -qi '^x-content-type-options: nosniff' "$response_headers"

request "$canonical_origin/"
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
if [ "$status" != 200 ]; then
    printf '%s\n' "canonical application did not return 200" >&2
    exit 1
fi
grep -Fq '<link rel="canonical" href="https://pwmtf.hyperchad.dev/"' "$response_body"
grep -Fq 'data-client-state="loading"' "$response_body"
grep -qi '^content-security-policy:' "$response_headers"
grep -qi '^cache-control:' "$response_headers"
if grep -qi '^cache-control:.*\bpublic\b' "$response_headers"; then
    printf '%s\n' "canonical application response is publicly cacheable" >&2
    exit 1
fi
grep -qi '^referrer-policy: same-origin' "$response_headers"
grep -qi '^cross-origin-opener-policy: same-origin' "$response_headers"
grep -qi '^cross-origin-resource-policy: same-origin' "$response_headers"
grep -qi '^x-content-type-options: nosniff' "$response_headers"

# The private integrity manifest must never be served from the public origin.
"$curl_bin" \
    --silent \
    --show-error \
    --connect-timeout "$max_time" \
    --max-time "$max_time" \
    --max-redirs 0 \
    --dump-header "$response_headers" \
    --output "$response_body" \
    "$canonical_origin/pwmtf-bundle-manifest.json"
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
if [ "$status" != 404 ]; then
    printf '%s\n' "private bundle manifest returned $status instead of 404" >&2
    exit 1
fi
if [ -s "$response_body" ]; then
    printf '%s\n' "private bundle manifest returned a response body" >&2
    exit 1
fi

# Exercise the production OIDC authorization entry point without following the
# provider redirect or exposing its opaque query values.
"$curl_bin" \
    --silent \
    --show-error \
    --connect-timeout "$max_time" \
    --max-time "$max_time" \
    --max-redirs 0 \
    --request POST \
    --header "Origin: $canonical_origin" \
    --dump-header "$response_headers" \
    --output "$response_body" \
    "$canonical_origin/auth/google/start"
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
location=$(awk 'BEGIN { IGNORECASE=1 } /^location:/ { sub(/^[^:]+:[[:space:]]*/, ""); sub(/\r$/, ""); print; exit }' "$response_headers")
if [ "$status" != 303 ]; then
    printf '%s\n' "Google authorization start returned $status instead of 303" >&2
    exit 1
fi
case "$location" in
    https://accounts.google.com/*) ;;
    *)
        printf '%s\n' "Google authorization start did not redirect to the verified provider" >&2
        exit 1
        ;;
esac
case "$location" in
    *redirect_uri=https%3A%2F%2Fpwmtf.hyperchad.dev%2Fauth%2Fgoogle%2Fcallback*|*redirect_uri=https://pwmtf.hyperchad.dev/auth/google/callback*) ;;
    *)
        printf '%s\n' "Google authorization start omitted the exact canonical callback" >&2
        exit 1
        ;;
esac
for required_parameter in 'client_id=' 'response_type=code' 'scope=' 'profile' 'state=' 'nonce=' 'code_challenge=' 'code_challenge_method=S256'; do
    case "$location" in
        *"$required_parameter"*) ;;
        *)
            printf '%s\n' "Google authorization start omitted a required authorization-code, state, nonce, or S256 PKCE parameter" >&2
            exit 1
            ;;
    esac
done
if ! grep -qi '^set-cookie: __Host-pwmtf_oidc=' "$response_headers"; then
    printf '%s\n' 'Google authorization start omitted the __Host-pwmtf_oidc binding cookie' >&2
    exit 1
fi
grep -qi '^set-cookie: .*Secure' "$response_headers"
grep -qi '^set-cookie: .*HttpOnly' "$response_headers"
grep -qi '^set-cookie: .*SameSite=Lax' "$response_headers"
grep -qi '^set-cookie: .*Path=/' "$response_headers"
grep -qi '^set-cookie: .*Max-Age=600' "$response_headers"
grep -qi '^set-cookie: .*Priority=High' "$response_headers"
if grep -qi '^set-cookie: .*Domain=' "$response_headers"; then
    printf '%s\n' "Google authorization binding cookie is not host-only" >&2
    exit 1
fi

# The callback boundary must reject malformed values before any provider exchange
# and must never reflect opaque input into the response body.
malformed_callback_marker=pwmtf-malformed-callback-marker
"$curl_bin" \
    --silent \
    --show-error \
    --connect-timeout "$max_time" \
    --max-time "$max_time" \
    --max-redirs 0 \
    --dump-header "$response_headers" \
    --output "$response_body" \
    "$canonical_origin/auth/google/callback?code=$malformed_callback_marker&state="
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
if [ "$status" != 400 ]; then
    printf '%s\n' "malformed Google callback returned $status instead of 400" >&2
    exit 1
fi
if grep -Fq "$malformed_callback_marker" "$response_body"; then
    printf '%s\n' "malformed Google callback reflected opaque input" >&2
    exit 1
fi

# Unauthenticated APIs must remain closed on the public production origin.
"$curl_bin" \
    --silent \
    --show-error \
    --connect-timeout "$max_time" \
    --max-time "$max_time" \
    --max-redirs 0 \
    --dump-header "$response_headers" \
    --output "$response_body" \
    "$canonical_origin/api/session"
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
if [ "$status" != 401 ]; then
    printf '%s\n' "unauthenticated session endpoint returned $status instead of 401" >&2
    exit 1
fi
if ! printf '%s' 'request is not authenticated' | cmp -s - "$response_body"; then
    printf '%s\n' "unauthenticated session endpoint returned an unexpected response body" >&2
    exit 1
fi

# State-changing social and invitation endpoints must reject unauthenticated
# requests before accepting or disclosing any workflow state.
for protected_path in challenges invitations invitations/redeem; do
    "$curl_bin" \
        --silent \
        --show-error \
        --connect-timeout "$max_time" \
        --max-time "$max_time" \
        --max-redirs 0 \
        --request POST \
        --header "Origin: $canonical_origin" \
        --header 'Content-Type: application/json' \
        --data '{}' \
        --dump-header "$response_headers" \
        --output "$response_body" \
        "$canonical_origin/api/$protected_path"
    status=$(awk 'NR == 1 { print $2 }' "$response_headers")
    if [ "$status" != 401 ]; then
        printf '%s\n' "unauthenticated $protected_path endpoint returned $status instead of 401" >&2
        exit 1
    fi
    if ! printf '%s' 'request is not authenticated' | cmp -s - "$response_body"; then
        printf '%s\n' "unauthenticated $protected_path endpoint returned an unexpected response body" >&2
        exit 1
    fi
done

# The single session WebSocket is state-bearing and must reject an
# unauthenticated HTTP/1.1 upgrade at the handshake. `/ws` takes no query.
"$curl_bin" \
    --http1.1 \
    --silent \
    --show-error \
    --connect-timeout "$max_time" \
    --max-time "$max_time" \
    --max-redirs 0 \
    --header "Origin: $canonical_origin" \
    --header 'Connection: Upgrade' \
    --header 'Upgrade: websocket' \
    --header 'Sec-WebSocket-Version: 13' \
    --header 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
    --dump-header "$response_headers" \
    --output "$response_body" \
    "$canonical_origin/ws"
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
if [ "$status" != 401 ]; then
    printf '%s\n' "unauthenticated WebSocket upgrade returned $status instead of 401" >&2
    exit 1
fi
if ! printf '%s' 'request is not authenticated' | cmp -s - "$response_body"; then
    printf '%s\n' "unauthenticated WebSocket upgrade returned an unexpected response body" >&2
    exit 1
fi

if [ "$mode" = --origin-only ]; then
    printf '%s\n' "production origin smoke passed"
    exit 0
fi

"$curl_bin" \
    --silent \
    --show-error \
    --connect-timeout "$max_time" \
    --max-time "$max_time" \
    --max-redirs 0 \
    --dump-header "$response_headers" \
    --output "$response_body" \
    "$directory_url"
status=$(awk 'NR == 1 { print $2 }' "$response_headers")
location=$(awk 'BEGIN { IGNORECASE=1 } /^location:/ { sub(/^[^:]+:[[:space:]]*/, ""); sub(/\r$/, ""); print; exit }' "$response_headers")
location=${location%/}
if [ "$status" != 308 ]; then
    printf '%s\n' "games-directory redirect returned $status instead of 308" >&2
    exit 1
fi
if [ "$location" != "$expected_directory_target" ]; then
    printf '%s\n' "games-directory redirect target is not the canonical origin" >&2
    exit 1
fi

printf '%s\n' "production origin and directory redirect smoke passed"
