#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

if [ "${PWMTF_WASM_BUNDLE_LOCKED:-0}" != 1 ]; then
    exec ./scripts/with-wasm-bundle-lock.py -- "$0" "$@"
fi

if [ ! -f dist/pwmtf-bundle-manifest.json ]; then
    ./scripts/build-wasm.sh
fi
./scripts/verify-wasm-bundle.py dist

if [ -n "${PWMTF_SERVER_BIN:-}" ]; then
    server_bin=$PWMTF_SERVER_BIN
    if [ ! -x "$server_bin" ]; then
        printf '%s\n' "PWMTF_SERVER_BIN must name an executable server binary" >&2
        exit 1
    fi
else
    cargo build --package pwmtf_server --bin pwmtf-server
    server_bin=$root/target/debug/pwmtf-server
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/pwmtf-native-smoke.XXXXXX")
server_pid=
cleanup() {
    status=$?
    if [ -n "$server_pid" ]; then
        kill -TERM "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    if [ "$status" -ne 0 ] && [ -f "$tmp/server.log" ]; then
        printf '%s\n' "native deployment smoke server log:" >&2
        cat "$tmp/server.log" >&2
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

port=${PWMTF_NATIVE_SMOKE_PORT:-4191}
database_path=$tmp/pwmtf.db
build_id=$(python3 -c 'import json; print(json.load(open("dist/pwmtf-bundle-manifest.json"))["candidate"]["build_id"])')
source_hash=$(python3 -c 'import json; print(json.load(open("dist/pwmtf-bundle-manifest.json"))["candidate"]["source_hash"])')

start_server() {
    PWMTF_BIND="127.0.0.1:$port" \
    PWMTF_DATABASE_PATH="$database_path" \
    PWMTF_WEB_ROOT="$root/dist" \
    PWMTF_GOOGLE_CLIENT_ID="native-smoke-client" \
    PWMTF_GOOGLE_CLIENT_SECRET="native-smoke-secret" \
    PWMTF_EXPECTED_BUILD_ID="$build_id" \
    PWMTF_EXPECTED_SOURCE_HASH="$source_hash" \
    "$server_bin" >"$tmp/server.log" 2>&1 &
    server_pid=$!
}

wait_for_server() {
    attempt=0
    while ! curl --fail --silent --output /dev/null "http://127.0.0.1:$port/readyz"; do
        if ! kill -0 "$server_pid" 2>/dev/null; then
            cat "$tmp/server.log" >&2
            printf '%s\n' "native smoke server exited before becoming ready" >&2
            exit 1
        fi
        attempt=$((attempt + 1))
        if [ "$attempt" -ge 200 ]; then
            cat "$tmp/server.log" >&2
            printf '%s\n' "native smoke server did not become ready" >&2
            exit 1
        fi
        sleep 0.1
    done
}

stop_server() {
    kill -TERM "$server_pid"
    wait "$server_pid"
    server_pid=
}

start_server
wait_for_server

curl --fail --silent --dump-header "$tmp/health.headers" --output "$tmp/health.body" \
    "http://127.0.0.1:$port/healthz"
[ "$(cat "$tmp/health.body")" = ok ]
grep -qi '^cache-control: no-store' "$tmp/health.headers"
grep -qi '^content-security-policy:' "$tmp/health.headers"
grep -qi '^x-content-type-options: nosniff' "$tmp/health.headers"

curl --fail --silent --dump-header "$tmp/ready.headers" --output "$tmp/ready.body" \
    "http://127.0.0.1:$port/readyz"
[ "$(cat "$tmp/ready.body")" = ready ]
grep -qi '^cache-control: no-store' "$tmp/ready.headers"
grep -qi '^content-security-policy:' "$tmp/ready.headers"
grep -qi '^x-content-type-options: nosniff' "$tmp/ready.headers"

curl --fail --silent --dump-header "$tmp/index.headers" --output "$tmp/index.body" \
    "http://127.0.0.1:$port/"
cmp dist/index.html "$tmp/index.body"
grep -qi '^cache-control: no-cache' "$tmp/index.headers"
grep -qi '^content-security-policy:' "$tmp/index.headers"

curl --fail --silent --output "$tmp/spa.body" \
    "http://127.0.0.1:$port/matches/reconnect-example"
cmp dist/index.html "$tmp/spa.body"

for protected_path in api/unknown auth/unknown; do
    curl --silent --dump-header "$tmp/protected.headers" --output "$tmp/protected.body" \
        "http://127.0.0.1:$port/$protected_path"
    [ "$(awk 'NR == 1 { print $2 }' "$tmp/protected.headers")" = 404 ]
    if grep -q 'data-client-state="loading"' "$tmp/protected.body"; then
        printf '%s\n' "native server returned the SPA for protected path $protected_path" >&2
        exit 1
    fi
done

for asset in bootstrap.js feasibility.js brand-mark.svg pwmtf_client_bg.wasm; do
    curl --fail --silent --output "$tmp/$asset" "http://127.0.0.1:$port/$asset"
    cmp "dist/$asset" "$tmp/$asset"
done

curl --silent --dump-header "$tmp/manifest.headers" --output "$tmp/manifest.body" \
    "http://127.0.0.1:$port/pwmtf-bundle-manifest.json"
[ "$(awk 'NR == 1 { print $2 }' "$tmp/manifest.headers")" = 404 ]
if grep -q '"candidate"' "$tmp/manifest.body"; then
    printf '%s\n' "native server exposed the private integrity manifest" >&2
    exit 1
fi

stop_server
if [ -n "${PWMTF_NATIVE_SMOKE_SEED_BIN:-}" ]; then
    seed_bin=$PWMTF_NATIVE_SMOKE_SEED_BIN
    if [ ! -x "$seed_bin" ]; then
        printf '%s\n' "PWMTF_NATIVE_SMOKE_SEED_BIN must name an executable seed binary" >&2
        exit 1
    fi
else
    cargo build --package pwmtf_server --example seed_native_smoke
    seed_bin=$root/target/debug/examples/seed_native_smoke
fi
"$seed_bin" "$database_path" >"$tmp/seed.out"
match_id=$(awk -F= '$1 == "match_id" { print $2 }' "$tmp/seed.out")
live_match_id=$(awk -F= '$1 == "live_match_id" { print $2 }' "$tmp/seed.out")
live_command=$(awk -F= '$1 == "live_command" { print $2 }' "$tmp/seed.out")
live_terminal_command=$(awk -F= '$1 == "live_terminal_command" { print $2 }' "$tmp/seed.out")
player_one_cookie=$(awk -F= '$1 == "player_one_cookie" { print substr($0, index($0, "=") + 1) }' "$tmp/seed.out")
player_two_cookie=$(awk -F= '$1 == "player_two_cookie" { print substr($0, index($0, "=") + 1) }' "$tmp/seed.out")
[ "$match_id" = 7001 ]
[ "$live_match_id" = 7002 ]
[ -n "$live_command" ]
[ -n "$live_terminal_command" ]
[ -n "$player_one_cookie" ]
[ -n "$player_two_cookie" ]

start_server
wait_for_server

curl --fail --silent --output "$tmp/restarted-ready.body" \
    "http://127.0.0.1:$port/readyz"
[ "$(cat "$tmp/restarted-ready.body")" = ready ]
curl --fail --silent --output "$tmp/restarted-index.body" \
    "http://127.0.0.1:$port/"
cmp dist/index.html "$tmp/restarted-index.body"
# The browser session protocol: one binary `/ws` per tab, Hello first.
node - "$port" "$match_id" "$live_match_id" "$player_one_cookie" "$player_two_cookie" <<'JS'
import { pathToFileURL } from "node:url";
const session = await import(new URL("scripts/pwmtf-session-client.mjs", pathToFileURL(`${process.cwd()}/`)));
const [port, completedMatchId, liveMatchId, playerOneCookie, playerTwoCookie] = process.argv.slice(2);
const closesWithin = (client, ms = 3_000) => Promise.race([
  client.closed.then(() => true),
  new Promise((resolve) => setTimeout(() => resolve(false), ms)),
]);

const anonymous = await session.connect({ port, label: "unauthenticated" });
if (anonymous.status !== 401) {
  throw new Error(`unauthenticated /ws upgrade returned ${anonymous.status} instead of 401`);
}
const foreignOrigin = await session.connect({
  port, cookie: playerOneCookie, origin: "https://attacker.example", label: "foreign origin",
});
if (foreignOrigin.status === 101) {
  foreignOrigin.client.close();
  throw new Error("cross-origin /ws upgrade was accepted");
}

for (const [label, violate] of [
  ["text first frame", (client) => client.sendText("1")],
  ["non-Hello first frame", (client) => client.send(session.subscribeMatch(liveMatchId))],
  ["empty Hello", (client) => client.send(Buffer.from([0x01, 0x00]))],
  ["unsupported-only Hello", (client) => client.send(session.hello([1]))],
]) {
  const opened = await session.connect({ port, cookie: playerOneCookie, label });
  if (opened.status !== 101) throw new Error(`${label}: upgrade failed with ${opened.status}`);
  violate(opened.client);
  if (!(await closesWithin(opened.client))) {
    opened.client.close();
    throw new Error(`server kept the socket open after a ${label}`);
  }
  if (opened.client.drain().some((message) => message.type === "match-state")) {
    throw new Error(`${label} leaked match state`);
  }
}
for (const [label, violate] of [
  ["post-handshake text frame", (client) => client.sendText("1")],
  ["empty binary frame", (client) => client.send(Buffer.alloc(0))],
  ["unknown client tag", (client) => client.send(Buffer.from([0x7f]))],
  ["trailing bytes", (client) => client.send(Buffer.concat([session.ping(1), Buffer.from([0])]))],
  ["oversize frame", (client) => client.send(Buffer.alloc(513, 0x08))],
]) {
  const client = await session.openSession({ port, cookie: playerOneCookie, label });
  violate(client);
  if (!(await closesWithin(client))) {
    client.close();
    throw new Error(`server kept the socket open after a ${label}`);
  }
}

const one = await session.openSession({ port, cookie: playerOneCookie, label: "player one" });
const two = await session.openSession({ port, cookie: playerTwoCookie, label: "player two" });
try {
  one.send(session.ping(77));
  const pong = await one.next((message) => message.type === "pong", 5_000, "Pong");
  if (pong.nonce !== 77n || pong.serverNowMs <= 0n) throw new Error("Pong did not echo the nonce with a server clock");

  const completedOne = await session.subscribeToMatch(one, completedMatchId);
  const completedTwo = await session.subscribeToMatch(two, completedMatchId);
  if (
    completedOne.seat !== 1 || completedTwo.seat !== 2 ||
    completedOne.revision !== 2 || completedTwo.revision !== 2 ||
    completedOne.deadlineAtMs !== null || completedTwo.deadlineAtMs !== null ||
    completedOne.checksum !== completedTwo.checksum ||
    completedOne.snapshot.length === 0
  ) {
    throw new Error("restarted completed match did not recover per-seat authoritative state");
  }
  const live = await session.subscribeToMatch(one, liveMatchId);
  if (live.seat !== 1 || live.revision !== 0 || live.deadlineAtMs === null || live.deadlineAtMs <= live.serverNowMs) {
    throw new Error("live match did not recover revision zero with a pending deadline");
  }
  one.send(session.subscribeMatch("999999"));
  const denied = await one.next((message) => message.type === "subscription-denied", 5_000, "SubscriptionDenied");
  if (denied.kind !== "match" || denied.id !== "999999") {
    throw new Error("unauthorized match subscription was not denied");
  }
} finally {
  one.close();
  two.close();
}
console.log("session handshake, origin, fail-closed framing, recovery and subscription authorization passed");
JS

node ./scripts/test-shot-preview.js "$port" "$live_match_id" "$player_one_cookie" "$player_two_cookie"

node - "$port" "$live_match_id" "$player_one_cookie" "$player_two_cookie" "$live_command" "$live_terminal_command" <<'JS'
import { pathToFileURL } from "node:url";
const session = await import(new URL("scripts/pwmtf-session-client.mjs", pathToFileURL(`${process.cwd()}/`)));
const [port, matchId, playerOneCookie, playerTwoCookie, liveCommand, liveTerminalCommand] = process.argv.slice(2);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const envelope = (hex) => Buffer.from(hex, "hex");
// CommandEnvelope v1: u16 protocol version, u64 expected revision, 16-byte id.
const commandIdHex = (bytes) => bytes.subarray(10, 26).toString("hex");
const withCommandId = (bytes, fill) => {
  const copy = Buffer.from(bytes);
  copy.fill(fill, 10, 26);
  return copy;
};
const stateAt = (revision, predicate = () => true) => (message) =>
  message.type === "match-state" && message.matchId === matchId && message.revision === revision && predicate(message);
const open = async (cookie, label) => {
  const client = await session.openSession({ port, cookie, label });
  return { client, initial: await session.subscribeToMatch(client, matchId) };
};

const firstCommand = envelope(liveCommand);
const terminalCommand = envelope(liveTerminalCommand);

// A command on a connection without that match subscription is refused and
// never applied; NotSubscribed is not followed by match state.
const unsubscribed = await session.openSession({ port, cookie: playerOneCookie, label: "unsubscribed sender" });
unsubscribed.send(session.command(matchId, firstCommand));
const notSubscribed = await unsubscribed.next((message) => message.type === "rejected", 5_000, "Rejected");
if (notSubscribed.reason !== "not-subscribed" || notSubscribed.commandId !== commandIdHex(firstCommand)) {
  throw new Error(`unsubscribed command was not refused as not-subscribed: ${notSubscribed.reason}`);
}
await unsubscribed.absent((message) => message.type === "match-state", 200, "MatchState after NotSubscribed");
unsubscribed.close();

// Delayed, retransmitted first command: accepted once, broadcast to both
// seats with the transition, and the identical retransmit is not an error.
const delayedRttMs = 200;
const jitterMs = 35;
const retransmissionAfterLossMs = 40;
const startedAt = performance.now();
const one = await open(playerOneCookie, "player one revision zero");
const two = await open(playerTwoCookie, "player two revision zero");
if (one.initial.revision !== 0 || two.initial.revision !== 0 || one.initial.seat !== 1 || two.initial.seat !== 2) {
  throw new Error("participants did not subscribe at revision zero in their own seats");
}
await sleep(delayedRttMs + jitterMs + retransmissionAfterLossMs);
one.client.send(session.command(matchId, firstCommand));
one.client.send(session.command(matchId, firstCommand));
const broadcastOne = await one.client.next(stateAt(1, (message) => message.lastCommand !== null), 5_000, "revision one broadcast");
const broadcastTwo = await two.client.next(stateAt(1, (message) => message.lastCommand !== null), 5_000, "observer revision one");
const duplicateReply = await one.client.next(stateAt(1, (message) => message.lastCommand === null), 5_000, "duplicate reply");
for (const [label, state] of [["sender", broadcastOne], ["observer", broadcastTwo]]) {
  const last = state.lastCommand;
  if (
    last.commandId !== commandIdHex(firstCommand) ||
    last.version !== 1 ||
    last.command.kind !== "play-shot" ||
    !(last.playbackTicks > 0)
  ) {
    throw new Error(`${label} broadcast did not describe the accepted transition: ${JSON.stringify(last, (_, v) => typeof v === "bigint" ? v.toString() : v)}`);
  }
}
if (
  broadcastOne.checksum !== broadcastTwo.checksum ||
  broadcastOne.checksum !== duplicateReply.checksum ||
  broadcastTwo.seat !== 2 ||
  broadcastOne.deadlineAtMs === null ||
  broadcastOne.deadlineAtMs !== broadcastTwo.deadlineAtMs ||
  performance.now() - startedAt < delayedRttMs + jitterMs + retransmissionAfterLossMs
) {
  throw new Error("participants did not converge at revision one after impairment");
}
await one.client.absent((message) => message.type === "rejected", 150, "Rejected for an identical retransmit");
await two.client.absent(stateAt(2), 100, "a second transition from the retransmit");
one.client.close();
two.client.close();

// Reconnect: current state only, no transition replay, and a retransmit
// after reconnect is still accepted exactly once.
const reconnected = await open(playerTwoCookie, "player two revision one reconnect");
if (reconnected.initial.revision !== 1 || reconnected.initial.lastCommand !== null) {
  throw new Error("reconnected participant did not receive current revision one");
}
reconnected.client.close();
const retransmit = await open(playerOneCookie, "player one retransmit after reconnect");
retransmit.client.send(session.command(matchId, firstCommand));
const retransmitReply = await retransmit.client.next(stateAt(1), 5_000, "retransmit reply");
if (retransmitReply.lastCommand !== null) throw new Error("retransmit after reconnect was applied again");
await retransmit.client.absent((message) => message.type === "rejected", 150, "Rejected for a reconnect retransmit");

// A different command against the old revision is stale: Rejected with its
// reason, then a fresh MatchState for recovery.
const staleCommand = withCommandId(firstCommand, 0x55);
retransmit.client.send(session.command(matchId, staleCommand));
const stale = await retransmit.client.next((message) => message.type === "rejected", 5_000, "stale Rejected");
if (stale.reason !== "stale-revision" || stale.commandId !== commandIdHex(staleCommand) || stale.matchId !== matchId) {
  throw new Error(`stale command was not rejected as stale-revision: ${stale.reason}`);
}
const recovered = await retransmit.client.next(stateAt(1), 5_000, "MatchState after rejection");
if (recovered.lastCommand !== null) throw new Error("rejection recovery state carried a transition");
// Reusing an accepted id with different bytes is an idempotency conflict.
const conflicting = Buffer.from(terminalCommand);
conflicting.fill(0x53, 10, 26);
retransmit.client.send(session.command(matchId, conflicting));
const conflict = await retransmit.client.next((message) => message.type === "rejected", 5_000, "conflict Rejected");
if (!["idempotency-conflict", "wrong-turn", "illegal-move"].includes(conflict.reason)) {
  throw new Error(`reused command id with different bytes was rejected as ${conflict.reason}`);
}
await retransmit.client.next(stateAt(1), 5_000, "MatchState after conflict");
retransmit.client.close();
await sleep(100);

// Terminal command from the other seat with a lost first copy and duplicate.
const terminalStartedAt = performance.now();
const terminalBaseDelayMs = 75;
const terminalJitterMs = 20;
const terminal = await open(playerTwoCookie, "player two terminal command");
const terminalObserver = await open(playerOneCookie, "player one terminal observer");
await sleep(terminalBaseDelayMs + terminalJitterMs + retransmissionAfterLossMs);
terminal.client.send(session.command(matchId, terminalCommand));
terminal.client.send(session.command(matchId, terminalCommand));
const terminalState = await terminal.client.next(stateAt(2, (message) => message.lastCommand !== null), 5_000, "terminal broadcast");
const observedTerminal = await terminalObserver.client.next(stateAt(2, (message) => message.lastCommand !== null), 5_000, "observer terminal");
if (
  terminalState.lastCommand.command.kind !== "concede" ||
  terminalState.lastCommand.command.player !== 2 ||
  terminalState.lastCommand.playbackTicks !== 0 ||
  terminalState.deadlineAtMs !== null ||
  observedTerminal.checksum !== terminalState.checksum ||
  performance.now() - terminalStartedAt < terminalBaseDelayMs + terminalJitterMs + retransmissionAfterLossMs
) {
  throw new Error("command participant did not reach terminal revision two");
}
terminal.client.close();
terminalObserver.client.close();
await sleep(100);
for (const [cookie, label, seat] of [
  [playerOneCookie, "player one terminal reconnect", 1],
  [playerTwoCookie, "player two terminal reconnect", 2],
]) {
  const after = await open(cookie, label);
  after.client.close();
  if (after.initial.revision !== 2 || after.initial.seat !== seat || after.initial.deadlineAtMs !== null) {
    throw new Error("terminal reconnect did not receive authoritative revision two");
  }
}
console.log("session commands: NotSubscribed, broadcast transition, idempotent retransmit, stale/conflict reasons and terminal recovery passed");
JS

stop_server

if command -v sqlite3 >/dev/null 2>&1; then
    revision=$(sqlite3 -cmd '.timeout 5000' "$database_path" "SELECT canonical_revision FROM matches WHERE match_id = '7001'")
    commands=$(sqlite3 -cmd '.timeout 5000' "$database_path" "SELECT count(*) FROM accepted_commands WHERE match_id = '7001'")
    live_revision=$(sqlite3 -cmd '.timeout 5000' "$database_path" "SELECT canonical_revision FROM matches WHERE match_id = '7002'")
    live_commands=$(sqlite3 -cmd '.timeout 5000' "$database_path" "SELECT count(*) FROM accepted_commands WHERE match_id = '7002'")
    live_deadline=$(sqlite3 -cmd '.timeout 5000' "$database_path" "SELECT count(*) FROM matches WHERE match_id = '7002' AND deadline_revision IS NOT NULL")
    [ "$revision" = 2 ]
    [ "$commands" = 2 ]
    [ "$live_revision" = 2 ]
    [ "$live_commands" = 2 ]
    [ "$live_deadline" = 0 ]
fi

backup=$tmp/pwmtf-backup.db
restored=$tmp/pwmtf-restored.db
"$root/scripts/backup-database.sh" "$database_path" "$backup" >/dev/null
"$root/scripts/restore-database.sh" "$backup" "$restored" >/dev/null
database_path=$restored

start_server
wait_for_server
curl --fail --silent --output "$tmp/restored-ready.body" \
    "http://127.0.0.1:$port/readyz"
[ "$(cat "$tmp/restored-ready.body")" = ready ]
stop_server

if command -v sqlite3 >/dev/null 2>&1; then
    revision=$(sqlite3 -cmd '.timeout 5000' "$restored" "SELECT canonical_revision FROM matches WHERE match_id = '7001'")
    commands=$(sqlite3 -cmd '.timeout 5000' "$restored" "SELECT count(*) FROM accepted_commands WHERE match_id = '7001'")
    live_revision=$(sqlite3 -cmd '.timeout 5000' "$restored" "SELECT canonical_revision FROM matches WHERE match_id = '7002'")
    live_commands=$(sqlite3 -cmd '.timeout 5000' "$restored" "SELECT count(*) FROM accepted_commands WHERE match_id = '7002'")
    live_deadline=$(sqlite3 -cmd '.timeout 5000' "$restored" "SELECT count(*) FROM matches WHERE match_id = '7002' AND deadline_revision IS NOT NULL")
    [ "$revision" = 2 ]
    [ "$commands" = 2 ]
    [ "$live_revision" = 2 ]
    [ "$live_commands" = 2 ]
    [ "$live_deadline" = 0 ]
fi

printf '%s\n' "native deployment smoke passed"
