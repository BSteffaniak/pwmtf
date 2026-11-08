# Canonical physics selection

## Decision

PWMTF selects the focused fixed-point billiards solver now in `pwmtf_billiards` for physics profile version 1.

The selected solver uses integer micro-units, quantized external commands, fixed 240 Hz ticks, stable ball/pocket identifiers, stable pair iteration, canonical event sorting, complete versioned snapshots, and bounded shot/event limits. The profile is immutable and serialized into every canonical match snapshot.

## Evidence

The shared qualification corpus covers:

- Straight and glancing ball contacts.
- Chain and simultaneous contacts with stable identifier ordering.
- Side and corner-jaw cushion approaches.
- Side, object-ball, and cue-ball pockets, including scratch.
- Maximum-power break contact without front-ball tunnelling.
- Low-speed settling.
- Side and top/back spin response.
- Seeded rack constraints.
- Repeated replay equality.
- Moving-state serialization and rollback continuation.
- Native debug/release and `wasm32-unknown-unknown` compilation.

## Bounded Avian prototype

The Avian prototype is intentionally an ownership and dependency-boundary prototype rather than a second executable simulator. Mapping the existing corpus into Avian would require Bevy worlds, ECS components, entity/contact ordering normalization, and an adapter back into canonical `VersionedTableState` and `SimulationEvent` values. That adapter would duplicate the focused solver at the exact authority boundary where `INVARIANTS.md` forbids Bevy/ECS ownership.

The focused solver already passes every comparison criterion that an executable Avian prototype would need to reproduce: the same table and pocket geometry, side/top spin, chain and simultaneous contact ordering, complete snapshots, rollback continuation, native/WASM compilation, and release throughput. Avian offers no unresolved product-risk reduction against those criteria. Adding it solely to run duplicate fixtures would create a speculative dependency and second candidate authority path, contrary to repository package and validation guidance. The bounded prototype therefore rejects dependency introduction before runtime implementation.

## Rejected alternatives

### Constrained floating point

Rejected for the launch canonical profile because native/WASM transcendental and arithmetic behavior would require an additional normalization and cross-target proof boundary. Fixed point already satisfies current precision, replay, ordering, serialization, and performance needs without that ambiguity.

### Avian canonical simulation

Rejected for the launch canonical profile. An Avian prototype would introduce Bevy/ECS ownership and iteration-order risk at the exact boundary required to remain renderer-independent. The focused solver already implements the product-specific table, pockets, spin, stable contact ordering, rollback state, and event stream with no Bevy dependency. Adding Avian solely to duplicate the same corpus would not reduce a remaining product risk and would create a second candidate authority path.

Avian may still be used later for non-authoritative presentation experiments, but it cannot own accepted gameplay outcomes.

## Native performance evidence

On 2026-08-20, the release benchmark ran 12,000 complete corpus shots in 1,533 ms on the local Apple M4 development machine: 7,825.46 settled shots/second, checksum `04e74a68d60774a1`. This is strong native headroom for two-player turn-based authoritative simulation, but does not replace browser/device WASM runtime measurement.

## Revisit triggers

Reconsider the selected solver only if the complete product exposes a reproducible physics defect that cannot be corrected within the focused model, or measured release/WASM performance fails the target device budget. Any replacement must pass the same corpus and preserve canonical replay compatibility or introduce an explicitly versioned profile transition.
