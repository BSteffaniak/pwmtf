#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

assert_rejected() {
    label=$1
    if (
        cd "$tmp"
        "$root/scripts/check-architecture.sh"
    ) >/dev/null 2>&1; then
        printf '%s\n' "architecture checker failed to reject $label" >&2
        exit 1
    fi
}

reset_fixture() {
    rm -rf "$tmp/packages"
    mkdir -p "$tmp/packages/billiards/src" "$tmp/packages/server/src"
    printf '%s\n' 'pub struct SafeDomainType;' > "$tmp/packages/billiards/src/lib.rs"
    printf '%s\n' '[package]' 'name = "pwmtf_billiards"' > "$tmp/packages/billiards/Cargo.toml"
    printf '%s\n' '[package]' 'name = "pwmtf_server"' > "$tmp/packages/server/Cargo.toml"
}

reset_fixture
printf '%s\n' 'const QUERY: &str = "SELECT * FROM matches";' > "$tmp/packages/server/src/lib.rs"
assert_rejected "raw SQL"

reset_fixture
printf '%s\n' 'use bevy::prelude::*;' > "$tmp/packages/billiards/src/lib.rs"
assert_rejected "Bevy in the game domain"

reset_fixture
printf '%s\n' 'use switchy_database::Database;' > "$tmp/packages/billiards/src/lib.rs"
assert_rejected "persistence in the game domain"

reset_fixture
mkdir -p "$tmp/packages/sync/src"
printf '%s\n' '[package]' 'name = "pwmtf_sync"' > "$tmp/packages/sync/Cargo.toml"
printf '%s\n' 'use tokio::sync::Mutex;' > "$tmp/packages/sync/src/lib.rs"
assert_rejected "an async runtime in sync"

reset_fixture
mkdir -p "$tmp/packages/sync/src"
printf '%s\n' '[package]' 'name = "pwmtf_sync"' > "$tmp/packages/sync/Cargo.toml"
printf '%s\n' 'use pwmtf_eight_ball::MatchState;' > "$tmp/packages/sync/src/lib.rs"
assert_rejected "a specific game in sync"

reset_fixture
mkdir -p "$tmp/packages/common/src"
printf '%s\n' '[package]' 'name = "pwmtf_common"' > "$tmp/packages/common/Cargo.toml"
assert_rejected "a generic package directory"

reset_fixture
printf '%s\n' '[package]' 'name = "unprefixed"' > "$tmp/packages/server/Cargo.toml"
assert_rejected "an unprefixed package name"

reset_fixture
printf '%s\n' 'pub enum TimeoutOutcome { Forfeit }' > "$tmp/packages/billiards/src/lib.rs"
assert_rejected "timeout forfeiture behavior"

reset_fixture
printf '%s\n' 'pub struct CanonicalSnapshot { value: u8 }' > "$tmp/packages/billiards/src/lib.rs"
assert_rejected "an unversioned canonical payload marker"

reset_fixture
printf '%s\n' 'struct Row { session_token: String }' > "$tmp/packages/server/src/lib.rs"
assert_rejected "a plaintext session token persistence field"

reset_fixture
mkdir -p "$tmp/packages/client/web"
printf '%s\n' 'const url = `/ws?match_id=${matchId}`;' > "$tmp/packages/client/web/bootstrap.js"
assert_rejected "a query-string browser match socket"

reset_fixture
mkdir -p "$tmp/packages/client/web"
printf '%s\n' 'await fetch(`/api/matches/${matchId}`);' > "$tmp/packages/client/web/bootstrap.js"
assert_rejected "browser match access polling"

reset_fixture
mkdir -p "$tmp/packages/client/web"
printf '%s\n' 'apiRequest(`/api/lobbies/${id}/connections/${connection}`);' > "$tmp/packages/client/web/bootstrap.js"
assert_rejected "durable lobby connection heartbeats"

printf '%s\n' "architecture checker self-tests passed"
