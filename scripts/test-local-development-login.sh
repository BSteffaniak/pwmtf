#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
port=${PWMTF_LOCAL_LOGIN_TEST_PORT:-4397}
tmp=$(mktemp -d "${TMPDIR:-/tmp}/pwmtf-local-login.XXXXXX")
server_pid=
cleanup() {
    if [ -n "$server_pid" ]; then
        kill -TERM "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

cargo build -p pwmtf_server --features insecure --bin pwmtf-server
PWMTF_DEV_MODE=true \
PWMTF_CANONICAL_ORIGIN="http://127.0.0.1:$port" \
PWMTF_BIND="127.0.0.1:$port" \
PWMTF_DATABASE_PATH="$tmp/pwmtf.db" \
PWMTF_WEB_ROOT="$root/dist" \
    "$root/target/debug/pwmtf-server" >"$tmp/server.log" 2>&1 &
server_pid=$!

ready=false
for _ in $(seq 1 80); do
    if curl --fail --silent --show-error "http://127.0.0.1:$port/readyz" >/dev/null 2>&1; then
        ready=true
        break
    fi
    sleep 0.25
done
if [ "$ready" != true ]; then
    cat "$tmp/server.log" >&2
    exit 1
fi

curl --fail --silent --show-error "http://127.0.0.1:$port/api/auth/runtime" \
    | jq -e '.development_login == true and .google_login == false' >/dev/null
status=$(curl --silent --show-error --output "$tmp/google.body" --write-out '%{http_code}' \
    --request POST --header "Origin: http://127.0.0.1:$port" \
    "http://127.0.0.1:$port/auth/google/start")
[ "$status" = 404 ]

curl --fail --silent --show-error --dump-header "$tmp/headers" --output /dev/null \
    --cookie-jar "$tmp/cookies" --request POST \
    --header "Origin: http://127.0.0.1:$port" \
    --header "X-PWMTF-Origin: http://127.0.0.1:$port" \
    --header 'Content-Type: application/json' \
    --data '{"username":"local_one"}' \
    "http://127.0.0.1:$port/auth/development"
grep -qi '^set-cookie: pwmtf_dev_session=' "$tmp/headers"
if grep -qi '^set-cookie: .*Secure' "$tmp/headers"; then
    printf '%s\n' "development session cookie unexpectedly used Secure" >&2
    exit 1
fi
curl --fail --silent --show-error --cookie "$tmp/cookies" \
    "http://127.0.0.1:$port/api/session" \
    | jq -e '.handle == "local_one" and .development_login == true' >/dev/null

curl --fail --silent --show-error --dump-header "$tmp/headers-two" --output /dev/null \
    --cookie-jar "$tmp/cookies-two" --request POST \
    --header "Origin: http://127.0.0.1:$port" \
    --header "X-PWMTF-Origin: http://127.0.0.1:$port" \
    --header 'Content-Type: application/json' \
    --data '{"username":"local_two"}' \
    "http://127.0.0.1:$port/auth/development"
curl --fail --silent --show-error --cookie "$tmp/cookies-two" \
    "http://127.0.0.1:$port/api/session" \
    | jq -e '.handle == "local_two" and .development_login == true' >/dev/null

curl --fail --silent --show-error --cookie "$tmp/cookies" --request POST \
    --header "Origin: http://127.0.0.1:$port" \
    --header "X-PWMTF-Origin: http://127.0.0.1:$port" \
    --header 'Content-Type: application/json' \
    --data '{"handle":"local_two"}' \
    "http://127.0.0.1:$port/api/challenges" \
    | jq -e '.challenge_id | strings' >/dev/null
curl --fail --silent --show-error --cookie "$tmp/cookies-two" \
    "http://127.0.0.1:$port/api/challenges" \
    | jq -e 'length == 1 and .[0].from_handle == "local_one"' >/dev/null

invitation_url=$(curl --fail --silent --show-error --cookie "$tmp/cookies" \
    --request POST --header "Origin: http://127.0.0.1:$port" \
    --header "X-PWMTF-Origin: http://127.0.0.1:$port" \
    "http://127.0.0.1:$port/api/invitations" | jq -r '.invitation_url')
invitation_token=${invitation_url#*?invite=}
lobby_id=$(curl --fail --silent --show-error --cookie "$tmp/cookies-two" \
    --request POST --header "Origin: http://127.0.0.1:$port" \
    --header "X-PWMTF-Origin: http://127.0.0.1:$port" \
    --header 'Content-Type: application/json' \
    --data "{\"token\":\"$invitation_token\"}" \
    "http://127.0.0.1:$port/api/invitations/redeem" | jq -r '.lobby_id')
curl --fail --silent --show-error --cookie "$tmp/cookies" \
    "http://127.0.0.1:$port/api/lobbies" \
    | jq -e --arg lobby "$lobby_id" 'any(.[]; .lobby_id == $lobby and .status == "waiting")' >/dev/null
# Lobby presence is a live `/ws` connection subscribed to the lobby; ready
# stays HTTP, and the match starts only once both seats are present and ready.
cookie_one=$(sed -n 's/^[Ss]et-[Cc]ookie: \(pwmtf_dev_session=[^;]*\).*/\1/p' "$tmp/headers" | tr -d '\r')
cookie_two=$(sed -n 's/^[Ss]et-[Cc]ookie: \(pwmtf_dev_session=[^;]*\).*/\1/p' "$tmp/headers-two" | tr -d '\r')
[ -n "$cookie_one" ]
[ -n "$cookie_two" ]
match_id=$(cd "$root" && node - "$port" "$lobby_id" "$cookie_one" "$cookie_two" <<'JS'
import { pathToFileURL } from "node:url";
const session = await import(new URL("scripts/pwmtf-session-client.mjs", pathToFileURL(`${process.cwd()}/`)));
const [port, lobbyId, cookieOne, cookieTwo] = process.argv.slice(2);
const origin = `http://127.0.0.1:${port}`;
const ready = (cookie) => fetch(`${origin}/api/lobbies/${lobbyId}/ready`, {
  method: "POST",
  headers: { Cookie: cookie, Origin: origin, "X-PWMTF-Origin": origin, Accept: "application/json" },
});
const one = await session.openSession({ port, cookie: cookieOne, origin, label: "local one" });
const two = await session.openSession({ port, cookie: cookieTwo, origin, label: "local two" });
try {
  one.send(session.subscribeLobby(lobbyId));
  await new Promise((resolve) => setTimeout(resolve, 150));
  const firstReady = await ready(cookieOne);
  if (!firstReady.ok) throw new Error(`present player could not ready: ${firstReady.status}`);
  const firstLobby = await firstReady.json();
  if (firstLobby.status !== "waiting" || firstLobby.match_id !== null) {
    throw new Error("lobby started with only one present player");
  }
  await one.next((message) => message.type === "lobby-changed" && message.lobbyId === lobbyId, 5_000, "LobbyChanged");
  // Readiness without presence must not start the match.
  const absentReady = await ready(cookieTwo);
  if (absentReady.ok) {
    const absentLobby = await absentReady.json();
    if (absentLobby.status === "started" || absentLobby.match_id !== null) {
      throw new Error("lobby started while the second player had no live connection");
    }
  }
  two.send(session.subscribeLobby(lobbyId));
  if (!absentReady.ok) {
    await new Promise((resolve) => setTimeout(resolve, 150));
    const presentReady = await ready(cookieTwo);
    if (!presentReady.ok) throw new Error(`second present player could not ready: ${presentReady.status}`);
  }
  const started = await Promise.all([one, two].map((client) =>
    client.next((message) => message.type === "match-started" && message.lobbyId === lobbyId, 5_000, "MatchStarted")));
  if (started[0].matchId !== started[1].matchId) throw new Error("lobby members saw different started matches");
  const states = await Promise.all([one, two].map((client) => session.subscribeToMatch(client, started[0].matchId)));
  if (states[0].revision !== 0 || states[0].seat === states[1].seat || states[0].checksum !== states[1].checksum) {
    throw new Error("started match did not give both members distinct seats at revision zero");
  }
  console.log(started[0].matchId);
} finally {
  one.close();
  two.close();
}
JS
)
case "$match_id" in
    ''|*[!0-9]*) printf '%s\n' "lobby presence did not start a match" >&2; exit 1 ;;
esac
curl --fail --silent --show-error --cookie "$tmp/cookies" \
    "http://127.0.0.1:$port/api/lobbies" \
    | jq -e --arg lobby "$lobby_id" --arg match "$match_id" 'any(.[]; .lobby_id == $lobby and .status == "started" and .match_id == $match)' >/dev/null

printf '%s\n' "local username login, social discovery, lobby readiness, and match start acceptance passed"
