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

for asset in bootstrap.js brand-mark.svg pwmtf_client_bg.wasm; do
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
curl --fail --silent \
    --header "Cookie: $player_one_cookie" \
    --output "$tmp/restarted-match-one.body" \
    "http://127.0.0.1:$port/api/matches/$match_id"
grep -q '"player":1' "$tmp/restarted-match-one.body"
grep -q '"revision":2' "$tmp/restarted-match-one.body"
grep -q '"active_player":0' "$tmp/restarted-match-one.body"
grep -q '"completed":true' "$tmp/restarted-match-one.body"
grep -q '"deadline_at_ms":null' "$tmp/restarted-match-one.body"
grep -Eq '"server_time_ms":[0-9]+' "$tmp/restarted-match-one.body"
curl --fail --silent \
    --header "Cookie: $player_two_cookie" \
    --output "$tmp/restarted-match-two.body" \
    "http://127.0.0.1:$port/api/matches/$match_id"
grep -q '"player":2' "$tmp/restarted-match-two.body"
grep -q '"revision":2' "$tmp/restarted-match-two.body"
grep -q '"deadline_at_ms":null' "$tmp/restarted-match-two.body"

curl --fail --silent \
    --header "Cookie: $player_one_cookie" \
    --output "$tmp/live-match-zero.body" \
    "http://127.0.0.1:$port/api/matches/$live_match_id"
grep -q '"player":1' "$tmp/live-match-zero.body"
grep -q '"revision":0' "$tmp/live-match-zero.body"
grep -q '"active_player":1' "$tmp/live-match-zero.body"
grep -q '"completed":false' "$tmp/live-match-zero.body"
grep -Eq '"deadline_at_ms":[0-9]+' "$tmp/live-match-zero.body"
grep -Eq '"server_time_ms":[0-9]+' "$tmp/live-match-zero.body"

node ./scripts/test-shot-preview.js "$port" "$live_match_id" "$player_one_cookie" "$player_two_cookie"

node - "$port" "$live_match_id" "$player_one_cookie" "$player_two_cookie" "$live_command" "$live_terminal_command" <<'JS'
import net from "node:net";
import crypto from "node:crypto";
const [port, matchId, playerOneCookie, playerTwoCookie, liveCommand, liveTerminalCommand] = process.argv.slice(2);
function clientTextFrame(text) {
  const payload = Buffer.from(text);
  const mask = crypto.randomBytes(4);
  const frame = Buffer.alloc(6 + payload.length);
  frame[0] = 0x81;
  frame[1] = 0x80 | payload.length;
  mask.copy(frame, 2);
  for (let index = 0; index < payload.length; index += 1) {
    frame[6 + index] = payload[index] ^ mask[index % 4];
  }
  return frame;
}
function clientBinaryFrame(payload) {
  const mask = crypto.randomBytes(4);
  const headerLength = payload.length < 126 ? 6 : 8;
  const frame = Buffer.alloc(headerLength + payload.length);
  frame[0] = 0x82;
  if (payload.length < 126) {
    frame[1] = 0x80 | payload.length;
    mask.copy(frame, 2);
  } else {
    frame[1] = 0x80 | 126;
    frame.writeUInt16BE(payload.length, 2);
    mask.copy(frame, 4);
  }
  const payloadOffset = headerLength;
  for (let index = 0; index < payload.length; index += 1) {
    frame[payloadOffset + index] = payload[index] ^ mask[index % 4];
  }
  return frame;
}
function frames(buffer) {
  const decoded = [];
  let offset = 0;
  while (buffer.length - offset >= 2) {
    const opcode = buffer[offset] & 0x0f;
    let length = buffer[offset + 1] & 0x7f;
    let header = 2;
    if (length === 126) {
      if (buffer.length - offset < 4) break;
      length = buffer.readUInt16BE(offset + 2);
      header = 4;
    } else if (length === 127) {
      if (buffer.length - offset < 10) break;
      const large = buffer.readBigUInt64BE(offset + 2);
      if (large > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("oversized WebSocket frame");
      length = Number(large);
      header = 10;
    }
    if (buffer.length - offset < header + length) break;
    decoded.push({ opcode, payload: buffer.subarray(offset + header, offset + header + length) });
    offset += header + length;
  }
  return { decoded, remaining: buffer.subarray(offset) };
}
async function subscribe(
  cookie,
  {
    command = null,
    duplicateCommand = false,
    disconnectAfterInitial = false,
    disconnectBeforeCommand = false,
    delayMs = 0,
    jitterMs = 0,
    loseFirstCommand = false,
    expectRejected = false,
    label = "subscription",
  } = {},
) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(Number(port), "127.0.0.1");
    const timer = setTimeout(() => reject(new Error("WebSocket subscription timed out")), 5_000);
    let handshake = true;
    let buffer = Buffer.alloc(0);
    let negotiated = false;
    let rejected = false;
    let initialRevision = null;
    socket.on("connect", () => {
      const key = crypto.randomBytes(16).toString("base64");
      socket.write([
        `GET /ws?match_id=${matchId} HTTP/1.1`,
        `Host: 127.0.0.1:${port}`,
        "Upgrade: websocket",
        "Connection: Upgrade",
        `Sec-WebSocket-Key: ${key}`,
        "Sec-WebSocket-Version: 13",
        "Sec-WebSocket-Protocol: pwmtf",
        "User-Agent: pwmtf-native-smoke",
        "Origin: https://pwmtf.hyperchad.dev",
        `Cookie: ${cookie}`,
        "Cache-Control: no-cache",
        "Pragma: no-cache",
        "",
        "",
      ].join("\r\n"));
    });
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (handshake) {
        const end = buffer.indexOf("\r\n\r\n");
        if (end < 0) return;
        const response = buffer.subarray(0, end).toString();
        const contentLength = Number(response.match(/content-length: (\d+)/i)?.[1] ?? 0);
        buffer = buffer.subarray(end + 4);
        if (!response.startsWith("HTTP/1.1 101")) {
          if (buffer.length < contentLength) return;
          reject(new Error(`${label} WebSocket upgrade failed: ${response}\n${buffer.subarray(0, contentLength)}`));
          return;
        }
        handshake = false;
        socket.write(clientTextFrame("1"));
      }
      const parsed = frames(buffer);
      buffer = parsed.remaining;
      for (const frame of parsed.decoded) {
        if (!negotiated) {
          if (frame.opcode !== 1 || frame.payload.toString() !== "1") {
            reject(new Error("protocol negotiation failed"));
            return;
          }
          negotiated = true;
        } else if (frame.opcode === 1 && frame.payload.toString() === "rejected") {
          rejected = true;
        } else if (frame.opcode === 2 && frame.payload.length >= 22) {
          const revision = Number(frame.payload.readBigUInt64BE(2));
          if (expectRejected && rejected) {
            clearTimeout(timer);
            socket.end();
            resolve({ rejected: true, revision });
            return;
          }
          if (initialRevision === null) {
            initialRevision = revision;
            if (disconnectAfterInitial || disconnectBeforeCommand) {
              socket.end();
              setTimeout(() => {
                clearTimeout(timer);
                socket.destroy();
                resolve({ disconnected: true, revision });
              }, 50);
              return;
            }
            if (command !== null) {
              const commandFrame = clientBinaryFrame(Buffer.from(command, "hex"));
              const sendCommand = () => {
                socket.write(commandFrame);
                if (duplicateCommand) socket.write(commandFrame);
                socket.write(Buffer.from([0x82, 0x80, 0, 0, 0, 0])); // malformed/stale noise
              };
              const deliveryDelay = delayMs + jitterMs;
              if (loseFirstCommand) {
                setTimeout(sendCommand, deliveryDelay + retransmissionAfterLossMs);
              } else {
                setTimeout(sendCommand, deliveryDelay);
              }
            }
          } else if (revision > initialRevision) {
            clearTimeout(timer);
            socket.end();
            resolve({ length: frame.payload.length, revision });
          }
        }
      }
    });
    socket.on("error", reject);
  });
}
const delayedRttMs = 200;
const jitterMs = 35;
const retransmissionAfterLossMs = 40;
const startedAt = performance.now();
const firstTransition = await Promise.all([
  subscribe(playerOneCookie, {
    command: liveCommand,
    duplicateCommand: true,
    delayMs: delayedRttMs,
    jitterMs,
    loseFirstCommand: true,
    label: "player one revision zero",
  }),
  subscribe(playerTwoCookie, { label: "player two revision zero" }),
]);
const elapsed = performance.now() - startedAt;
const minimumImpairedDelayMs = delayedRttMs + jitterMs + retransmissionAfterLossMs;
if (
  firstTransition[0].length !== firstTransition[1].length ||
  firstTransition[0].revision !== 1 ||
  firstTransition[1].revision !== 1 ||
  elapsed < minimumImpairedDelayMs
) {
  throw new Error("participant subscriptions did not converge at revision one after impairment");
}
await new Promise((resolve) => setTimeout(resolve, 100));
const reconnected = await subscribe(playerTwoCookie, {
  disconnectAfterInitial: true,
  label: "player two revision one reconnect",
});
if (!reconnected.disconnected || reconnected.revision !== 1) {
  throw new Error("reconnected participant did not receive current revision one");
}
const staleRejected = await subscribe(playerOneCookie, {
  command: liveCommand,
  expectRejected: true,
  label: "stale command rejection recovery",
});
if (!staleRejected.rejected || staleRejected.revision !== 1) {
  throw new Error("rejected stale command did not receive current authority");
}
const disconnectedBeforeTerminal = await subscribe(playerOneCookie, {
  disconnectBeforeCommand: true,
  label: "player one revision one disconnect",
});
if (!disconnectedBeforeTerminal.disconnected || disconnectedBeforeTerminal.revision !== 1) {
  throw new Error("participant did not disconnect from authoritative revision one");
}
await new Promise((resolve) => setTimeout(resolve, 100));
const terminalBaseDelayMs = 75;
const terminalJitterMs = 20;
const terminalStartedAt = performance.now();
const terminal = await Promise.all([
  subscribe(playerTwoCookie, {
    command: liveTerminalCommand,
    duplicateCommand: true,
    delayMs: terminalBaseDelayMs,
    jitterMs: terminalJitterMs,
    loseFirstCommand: true,
    label: "player two terminal command",
  }),
]);
const minimumTerminalDelayMs = terminalBaseDelayMs + terminalJitterMs + retransmissionAfterLossMs;
if (terminal[0].revision !== 2 || performance.now() - terminalStartedAt < minimumTerminalDelayMs) {
  throw new Error("command participant did not reach terminal revision two");
}
await new Promise((resolve) => setTimeout(resolve, 100));
const terminalReconnects = [];
for (const [cookie, label] of [
  [playerOneCookie, "player one terminal reconnect"],
  [playerTwoCookie, "player two terminal reconnect"],
]) {
  terminalReconnects.push(await subscribe(cookie, { disconnectAfterInitial: true, label }));
}
if (terminalReconnects.some((result) => !result.disconnected || result.revision !== 2)) {
  throw new Error("terminal reconnect did not receive authoritative revision two");
}

JS

curl --fail --silent \
    --header "Cookie: $player_one_cookie" \
    --output "$tmp/live-match-terminal.body" \
    "http://127.0.0.1:$port/api/matches/$live_match_id"
grep -q '"player":1' "$tmp/live-match-terminal.body"
grep -q '"revision":2' "$tmp/live-match-terminal.body"
grep -q '"active_player":0' "$tmp/live-match-terminal.body"
grep -q '"completed":true' "$tmp/live-match-terminal.body"
grep -q '"deadline_at_ms":null' "$tmp/live-match-terminal.body"

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
