# Pool with More Than Friends Agent Guidelines

This file defines required architecture and validation for changes in this repository.

## Product direction

Pool with More Than Friends (PWMTF) is a mobile-browser-first, two-player online 8-ball game. The browser client may predict presentation immediately, but the native Rust server is always authoritative for accepted commands, physics, rules, deadlines, and results.

`INVARIANTS.md` is binding. Read its relevant sections before changing gameplay, networking, persistence, identity, or deployment behavior. Stop and report any requested conflict instead of silently weakening an invariant.

## Workspace organization

- Use a Cargo workspace with domain-driven crates under `packages/`.
- Add a crate only when implementation requires a concrete ownership or dependency boundary.
- Do not create generic `core`, `common`, `shared`, `utils`, or similar catch-all crates.
- Package names use the `pwmtf_` prefix and underscore naming.
- A sibling `models/` crate may contain shared serializable types only when consumers must avoid the owning implementation crate.
- Models crates must not contain business logic, persistence, transport handling, or service orchestration.

## Architecture

- Canonical physics belongs in `pwmtf_billiards` and 8-ball rules in `pwmtf_eight_ball`, both independent of Bevy, transport, HTTP, authentication, and persistence. Keep them free of PWMTF product concerns: other hosts (such as Cozy Lounge) embed them.
- Authoritative command acceptance, idempotency, deadlines and client replay belong in `pwmtf_sync`, which is generic over the game (`ReplayGame`) and sans-IO: no transport, persistence, identity, async runtime, or specific game. `pwmtf_server` and `pwmtf_client` are adapters over it.
- Protocol crates own bounded versioned wire representations, never gameplay decisions.
- The client owns input, rendering, audio, prediction, reconciliation, interpolation, and browser integration only.
- The server owns identity, authorization, authoritative scheduling, persistence, recovery, and operational entry points.
- Never accept client-reported ball positions, pockets, fouls, turns, deadlines, or results as authoritative.
- Canonical physics must not depend on Bevy ECS/query iteration order.
- Persistence must use Switchy schema and query builders; application-owned raw SQL is prohibited.
- Keep projections derived and rebuildable from canonical records.
- Keep `pool-with-more-than-friends-progress.md` local unless the user explicitly chooses to commit it.

## Rust conventions

- Use Rust edition 2024.
- Declare third-party dependencies once in the root workspace table with a full version, `default-features = false`, and narrow features.
- Leaf manifests use `workspace = true`.
- Prefer `BTreeMap` and `BTreeSet` where deterministic ordering matters.
- Every package exposes `fail-on-warnings = []` and enables:

```rust
#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
```

- Document public APIs and every possible error condition.
- Add `#[must_use]` to constructors and getters when useful, but not redundantly to functions returning `Result` or `Option`.
- Use `thiserror` for domain error types.

## Security and compatibility

- Quantize and bound all externally supplied gameplay input.
- Authenticate and authorize every state-changing operation and match subscription.
- Commands must be revision-checked and idempotent.
- Accepted gameplay commands must be durable before acknowledgement.
- Pin immutable rules and physics profile versions per match.
- Version canonical protocol and persistence payloads; reject unknown versions.
- Store only hashes of session and invitation tokens.
- Never log credentials, cookies, tokens, OIDC secrets/claims, provider picture URLs, or complete identity-linked shot payloads.

## Required validation

After relevant code or configuration changes, run:

1. `cargo fmt --check`
2. `cargo check --workspace --all-targets`
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo nextest run --workspace --no-fail-fast`
5. `cargo machete --with-metadata`
6. `cargo deny check`
7. `./scripts/check-architecture.sh`
8. `./scripts/test-architecture-checks.sh`

Use `cargo test --workspace` only when Nextest is unavailable and explain why. Also run relevant native release, WASM, browser, protocol, persistence, impairment, recovery, deployment, redirect, backup, and restore checks when those areas exist.

For docs-only changes, runtime validation is not required. If a required command cannot run, report why.

## Completion reporting

Summarize changed behavior and report every validation command with PASS, FAIL, or SKIPPED. Do not claim a progress-document checkbox unless its entire outcome and exit criteria are complete without compromise.
