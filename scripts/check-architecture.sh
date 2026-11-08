#!/bin/sh
set -eu

fail=0

report_violation() {
    printf '%s\n' "architecture violation: $1" >&2
    fail=1
}

check_pattern() {
    label=$1
    pattern=$2
    shift 2

    existing=
    for path in "$@"; do
        if [ -e "$path" ]; then
            existing="$existing $path"
        fi
    done
    if [ -n "$existing" ] && grep -R -n -E "$pattern" $existing --include='*.rs' --include='*.toml' --exclude-dir=target 2>/dev/null; then
        report_violation "$label"
    fi
}

if [ -d packages ]; then
    manifests=$(find packages -type f -name Cargo.toml -print)
    if [ -n "$manifests" ]; then
        for manifest in $manifests; do
            package_name=$(awk '/^\[package\]/{in_package=1; next} /^\[/{in_package=0} in_package && /^name[[:space:]]*=/{print; exit}' "$manifest")
            if [ -n "$package_name" ] && ! printf '%s\n' "$package_name" | grep -q -E '"pwmtf_[a-z0-9_]+"'; then
                printf '%s:%s\n' "$manifest" "$package_name"
                report_violation "package name without the pwmtf_ prefix"
            fi
        done
    fi

    if find packages -mindepth 1 -maxdepth 1 -type d \( -name core -o -name common -o -name shared -o -name utils \) -print | grep .; then
        report_violation "forbidden generic package directory"
    fi
fi

# The browser speaks only the binary session protocol through WASM exports:
# no match polling, query-string sockets, lobby leases, or per-match sockets.
check_web_pattern() {
    label=$1
    pattern=$2
    if [ -d packages/client/web ] && grep -R -n -E "$pattern" packages/client/web --include='*.js' --include='*.html' 2>/dev/null; then
        report_violation "$label"
    fi
}

check_web_pattern "legacy browser match transport" '(/ws\?|connect_match_socket|disconnect_match_socket|match_socket_ready|match_socket_needs_reconnect|match_socket_retry_delay_ms|match_has_pending_prediction|set_match_player|refreshMatchAccess|/connections/|/api/matches/\$\{matchId\}`)'

check_pattern "application-owned raw SQL" '(SELECT[[:space:]]|INSERT[[:space:]]+INTO|UPDATE[[:space:]]+[[:alnum:]_]+[[:space:]]+SET|DELETE[[:space:]]+FROM|CREATE[[:space:]]+TABLE|ALTER[[:space:]]+TABLE|DROP[[:space:]]+TABLE)' packages
for domain in packages/billiards packages/eight_ball; do
    check_pattern "game-domain infrastructure dependency" '(^|[^[:alnum:]_])(bevy|lightyear|switchy[_-][[:alnum:]_]*|actix[-_][[:alnum:]_]*|axum|database|http|websocket|tokio)([^[:alnum:]_]|$)' "$domain"
    check_pattern "game-domain client authority vocabulary" '(ClientAuthoritative|client_authoritative|ReportedBallState|reported_ball_state)' "$domain"
done
check_pattern "timeout forfeiture behavior" '(timeout|Timeout).*(forfeit|Forfeit|automatic_loss|AutomaticLoss)' packages
check_pattern "unversioned canonical payload marker" 'Canonical(Payload|Snapshot|Command)[[:space:]]*\{' packages
check_pattern "plaintext secret persistence field" '(session_token|invitation_token)[[:space:]]*:' packages/server

if [ "$fail" -ne 0 ]; then
    exit 1
fi

printf '%s\n' "architecture checks passed"
