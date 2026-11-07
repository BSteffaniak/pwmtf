#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/pwmtf-production-smoke-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

cat >"$tmp/curl" <<'SH'
#!/bin/sh
set -eu
headers=
body=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --dump-header)
            headers=$2
            shift 2
            ;;
        --output)
            body=$2
            shift 2
            ;;
        --connect-timeout|--max-time|--max-redirs|--request|--header|--data)
            shift 2
            ;;
        --http1.1|--fail|--silent|--show-error)
            shift
            ;;
        *)
            url=$1
            shift
            ;;
    esac
done
case "$url" in
    https://pwmtf.hyperchad.dev/healthz)
        printf 'HTTP/1.1 200 OK\r\nStrict-Transport-Security: max-age=63072000; includeSubDomains\r\nCache-Control: no-store\r\nReferrer-Policy: same-origin\r\nX-Content-Type-Options: nosniff\r\nPermissions-Policy: camera=(), geolocation=(), microphone=()\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Resource-Policy: same-origin\r\n\r\n' >"$headers"
        printf ok >"$body"
        ;;
    https://pwmtf.hyperchad.dev/readyz)
        printf 'HTTP/1.1 200 OK\r\nStrict-Transport-Security: max-age=31536000\r\nX-Content-Type-Options: nosniff\r\n\r\n' >"$headers"
        printf ready >"$body"
        ;;
    https://pwmtf.hyperchad.dev/)
        printf 'HTTP/1.1 200 OK\r\nContent-Security-Policy: default-src self\r\nCache-Control: %s\r\nReferrer-Policy: same-origin\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Resource-Policy: same-origin\r\nX-Content-Type-Options: nosniff\r\n\r\n' "${PWMTF_FAKE_APPLICATION_CACHE:-no-cache}" >"$headers"
        printf '%s' '<link rel="canonical" href="https://pwmtf.hyperchad.dev/" /><main data-client-state="loading">' >"$body"
        ;;
    https://pwmtf.hyperchad.dev/pwmtf-bundle-manifest.json)
        printf 'HTTP/1.1 %s Not Found\r\nContent-Type: text/plain\r\n\r\n' "${PWMTF_FAKE_MANIFEST_STATUS:-404}" >"$headers"
        printf '%s' "${PWMTF_FAKE_MANIFEST_BODY:-}" >"$body"
        ;;
    https://pwmtf.hyperchad.dev/auth/google/start)
        printf 'HTTP/1.1 %s Redirect\r\nLocation: %s\r\nSet-Cookie: %s=opaque; Path=/; Max-Age=600; Secure; HttpOnly; SameSite=Lax; Priority=High%s\r\n\r\n' "${PWMTF_FAKE_OIDC_STATUS:-307}" "${PWMTF_FAKE_OIDC_TARGET:-https://accounts.google.com/o/oauth2/v2/auth?client_id=client&redirect_uri=https%3A%2F%2Fpwmtf.hyperchad.dev%2Fauth%2Fgoogle%2Fcallback&response_type=code&scope=openid%20profile&state=opaque&nonce=opaque&code_challenge=opaque&code_challenge_method=S256}" "${PWMTF_FAKE_OIDC_COOKIE_NAME:-__Host-pwmtf_oidc}" "${PWMTF_FAKE_OIDC_COOKIE_EXTRA:-}" >"$headers"
        : >"$body"
        ;;
    https://pwmtf.hyperchad.dev/auth/google/callback\?code=pwmtf-malformed-callback-marker\&state=)
        printf 'HTTP/1.1 %s Bad Request\r\nContent-Type: text/plain\r\n\r\n' "${PWMTF_FAKE_CALLBACK_STATUS:-400}" >"$headers"
        printf '%s' "${PWMTF_FAKE_CALLBACK_BODY:-invalid request}" >"$body"
        ;;
    https://pwmtf.hyperchad.dev/api/session)
        printf 'HTTP/1.1 %s Unauthorized\r\nContent-Type: text/plain\r\n\r\n' "${PWMTF_FAKE_SESSION_STATUS:-401}" >"$headers"
        printf '%s' "${PWMTF_FAKE_SESSION_BODY-request is not authenticated}" >"$body"
        ;;
    https://pwmtf.hyperchad.dev/api/challenges|https://pwmtf.hyperchad.dev/api/invitations|https://pwmtf.hyperchad.dev/api/invitations/redeem)
        printf 'HTTP/1.1 %s Unauthorized\r\nContent-Type: text/plain\r\n\r\n' "${PWMTF_FAKE_PROTECTED_STATUS:-401}" >"$headers"
        printf '%s' "${PWMTF_FAKE_PROTECTED_BODY-request is not authenticated}" >"$body"
        ;;
    https://pwmtf.hyperchad.dev/ws)
        printf 'HTTP/1.1 %s Unauthorized\r\nContent-Type: text/plain\r\n\r\n' "${PWMTF_FAKE_WEBSOCKET_STATUS:-401}" >"$headers"
        printf '%s' "${PWMTF_FAKE_WEBSOCKET_BODY-request is not authenticated}" >"$body"
        ;;
    https://hyperchad.dev/games/pool-with-more-than-friends)
        printf 'HTTP/1.1 %s Redirect\r\nLocation: %s\r\n\r\n' "${PWMTF_FAKE_REDIRECT_STATUS:-308}" "${PWMTF_FAKE_REDIRECT_TARGET:-https://pwmtf.hyperchad.dev}" >"$headers"
        : >"$body"
        ;;
    *)
        printf '%s\n' "unexpected fake curl URL: $url" >&2
        exit 1
        ;;
esac
SH
chmod +x "$tmp/curl"

PWMTF_CURL_BIN="$tmp/curl" "$root/scripts/test-production-smoke.sh" >"$tmp/pass.out"
grep -q 'production origin and directory redirect smoke passed' "$tmp/pass.out"
PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_REDIRECT_STATUS=500 \
    "$root/scripts/test-production-smoke.sh" --origin-only >"$tmp/origin-pass.out"
grep -q 'production origin smoke passed' "$tmp/origin-pass.out"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_OIDC_COOKIE_NAME=pwmtf_oidc_binding \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' 'production smoke accepted the obsolete OIDC binding cookie name' >&2
    exit 1
fi
grep -q 'omitted the __Host-pwmtf_oidc binding cookie' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_APPLICATION_CACHE='public, max-age=3600' \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted publicly cacheable application HTML" >&2
    exit 1
fi
grep -q 'canonical application response is publicly cacheable' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_MANIFEST_STATUS=200 \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted an exposed bundle manifest" >&2
    exit 1
fi
grep -q 'private bundle manifest returned 200 instead of 404' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_MANIFEST_BODY=manifest-data \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted leaked bundle metadata" >&2
    exit 1
fi
grep -q 'private bundle manifest returned a response body' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_REDIRECT_STATUS=302 \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted a temporary redirect" >&2
    exit 1
fi
grep -q 'returned 302 instead of 308' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_REDIRECT_TARGET=https://evil.example \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted a noncanonical redirect target" >&2
    exit 1
fi
grep -q 'target is not the canonical origin' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_OIDC_STATUS=302 \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted an unexpected OIDC redirect status" >&2
    exit 1
fi
grep -q 'Google authorization start returned 302 instead of 307' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_OIDC_TARGET=https://evil.example \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted a non-Google OIDC target" >&2
    exit 1
fi
grep -q 'did not redirect to the verified provider' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_OIDC_TARGET='https://accounts.google.com/o/oauth2/v2/auth?client_id=client&redirect_uri=https%3A%2F%2Fevil.example%2Fcallback&response_type=code&scope=profile&state=opaque&nonce=opaque&code_challenge=opaque&code_challenge_method=S256' \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted a noncanonical OIDC callback" >&2
    exit 1
fi
grep -q 'omitted the exact canonical callback' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_OIDC_COOKIE_EXTRA='; Domain=hyperchad.dev' \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted a domain-scoped OIDC binding cookie" >&2
    exit 1
fi
grep -q 'Google authorization binding cookie is not host-only' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_OIDC_TARGET='https://accounts.google.com/o/oauth2/v2/auth?client_id=client&redirect_uri=https%3A%2F%2Fpwmtf.hyperchad.dev%2Fauth%2Fgoogle%2Fcallback&response_type=code&scope=profile&state=opaque&nonce=opaque' \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted OIDC without S256 PKCE" >&2
    exit 1
fi
grep -q 'omitted a required authorization-code, state, nonce, or S256 PKCE parameter' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_CALLBACK_STATUS=200 \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted a malformed OIDC callback" >&2
    exit 1
fi
grep -q 'malformed Google callback returned 200 instead of 400' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_CALLBACK_BODY=pwmtf-malformed-callback-marker \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted callback input reflection" >&2
    exit 1
fi
grep -q 'malformed Google callback reflected opaque input' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_SESSION_STATUS=200 \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted an open session endpoint" >&2
    exit 1
fi
grep -q 'unauthenticated session endpoint returned 200 instead of 401' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_SESSION_BODY=identity-data \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted unauthenticated identity data" >&2
    exit 1
fi
grep -q 'unauthenticated session endpoint returned an unexpected response body' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_PROTECTED_STATUS=200 \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted an open protected social endpoint" >&2
    exit 1
fi
grep -q 'endpoint returned 200 instead of 401' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_PROTECTED_BODY=workflow-data \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted leaked social workflow data" >&2
    exit 1
fi
grep -q 'endpoint returned an unexpected response body' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_WEBSOCKET_STATUS=101 \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted an unauthenticated WebSocket" >&2
    exit 1
fi
grep -q 'unauthenticated WebSocket upgrade returned 101 instead of 401' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_FAKE_WEBSOCKET_BODY=subscription-data \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted leaked WebSocket data" >&2
    exit 1
fi
grep -q 'unauthenticated WebSocket upgrade returned an unexpected response body' "$tmp/rejected.err"

if PWMTF_CURL_BIN="$tmp/curl" PWMTF_CANONICAL_ORIGIN=https://evil.example \
    "$root/scripts/test-production-smoke.sh" >"$tmp/rejected.out" 2>"$tmp/rejected.err"; then
    printf '%s\n' "production smoke accepted a noncanonical application origin" >&2
    exit 1
fi
grep -q 'must be the canonical production origin' "$tmp/rejected.err"

printf '%s\n' "production smoke self-tests passed"
