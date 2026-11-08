# Pool with More Than Friends

Pool with More Than Friends (PWMTF) is a mobile-browser-first, two-player online 8-ball game written primarily in Rust.

The planned production experience uses a Bevy WebAssembly client with immediate local shot prediction and a native Rust server that remains authoritative for physics, rules, timers, and results. Players sign in with Google and start private games through exact-handle challenges or single-use invitation links.

## Status

The repository now contains the renderer-independent canonical game domain, bounded protocol, authoritative native server, Switchy persistence, and Bevy WebAssembly client. Core gameplay, social/lobby/rematch flows, prediction/reconciliation, recovery, deployment packaging, and qualification tooling are implemented; live infrastructure, physical mobile acceptance, final presentation/audio polish, and disputed launch-rule evidence remain open in the local progress document.

## Architecture

- Domain-driven Cargo packages live under `packages/` and use the `pwmtf_` prefix.
- Canonical gameplay and physics remain independent of rendering and infrastructure.
- Browser presentation may predict and reconcile, but never becomes authoritative.
- Persistence uses Switchy builders and local Turso on a Fly Volume initially.
- The canonical production origin will be `https://pwmtf.hyperchad.dev`.

Read `INVARIANTS.md` for durable architecture and `AGENTS.md` for contributor requirements.

## Development

The workspace contains `pwmtf_billiards` (deterministic physics), `pwmtf_eight_ball` (rules), `pwmtf_protocol`, `pwmtf_server`, and `pwmtf_client`. Use:

```sh
cargo fmt --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-fail-fast
cargo machete --with-metadata
cargo deny check
./scripts/check-native.sh
./scripts/check-wasm.sh
./scripts/build-wasm.sh
./scripts/test-wasm-source-hash.py
./scripts/serve-wasm-bundle.py --port 8080 --directory dist
./scripts/report-wasm-size.sh
./scripts/serve-feasibility.sh
./scripts/validate-feasibility-matrix.sh
./scripts/test-feasibility-tools.sh
./scripts/test-feasibility-matrix-lock.sh
./scripts/test-feasibility-server.sh
./scripts/test-wasm-bundle-integrity.sh
./scripts/test-wasm-bundle-lock.py
./scripts/test-wasm-bundle-concurrency.sh
./scripts/test-wasm-bundle-server.sh
./scripts/test-wasm-reproducible-build.sh
./scripts/test-wasm-size-evidence.sh
./scripts/write-wasm-size-evidence.py --bundle dist --output /tmp/pwmtf-wasm-size-evidence.json
./scripts/test-browser-telemetry.js
./scripts/test-browser-smoke.sh
./scripts/test-edge-smoke.sh
./scripts/test-firefox-smoke.sh
./scripts/test-safari-smoke.sh
./scripts/test-production-smoke.sh
./scripts/test-production-smoke-self.sh
./scripts/test-native-deployment-smoke.sh
./scripts/test-backup-restore.sh
./scripts/check-architecture.sh
./scripts/test-architecture-checks.sh
```

With Nix:

```sh
nix develop
```

### Local two-player login

PWMTF has an explicitly gated username-only login for manual local browser testing
without Google credentials:

```sh
./scripts/build-wasm.sh
PWMTF_DEV_MODE=true \
PWMTF_CANONICAL_ORIGIN=http://127.0.0.1:8080 \
PWMTF_BIND=127.0.0.1:8080 \
PWMTF_DATABASE_PATH=./pwmtf.db \
PWMTF_WEB_ROOT=./dist \
cargo run -p pwmtf_server --features insecure --bin pwmtf-server
```

Open two browser profiles, enter different lowercase usernames, and use the
normal challenge/invitation UI. These usernames create stable local handles and
normal hash-only sessions, not Google identities.

The route is compiled only by the non-default `insecure` feature. Startup also
requires `PWMTF_DEV_MODE=true`, no Google credentials, and an exact loopback HTTP
origin. An insecure build without development mode and a development-mode normal
build both fail closed. Production builds cannot compile the route, require
Google credentials, and retain Secure `__Host-` cookies.

The browser client discovers authentication capabilities from the server before
showing either login form. It never exposes the local form against a normal
production build and hides Google when development mode is active.

## License

PWMTF is licensed under the Mozilla Public License 2.0. See `LICENSE`.
