# PWMTF planar physics

Deterministic, integer fixed-point 2D physics for live table games such as
air hockey, foosball, bumper carts, marble races, pinball and paper-ball
basketball. A native authoritative server and WASM clients compute the same
bits, so states can be predicted, rolled back and verified by checksum (see
the tick mode of `pwmtf_sync`).

- **Dynamic discs** (pucks, balls, carts approximated as discs) with mass,
  restitution, linear damping and constant friction deceleration.
- **Kinematic discs** driven toward a target at a bounded speed and held in a
  confinement rectangle: a pointer-driven mallet or rod sets its target each
  tick and pushes dynamic discs as if infinitely heavy.
- **Static wall segments** and rectangular arena bounds.
- **Sensors** (rectangles or circles) that report a body's center entering or
  leaving exactly once, such as goals.
- **Layers and masks** to filter which bodies collide or are sensed.
- **Bounded substeps**: each tick is split so no body moves more than half the
  smallest radius per substep; `WorldConfig::min_radius` rejects bodies too
  small to be tunnelling-safe at the world's speed limit.
- **Canonical events**: contacts (merged per pair per tick, with approach
  speed for audio) and sensor changes in a fixed order.
- **Versioned encoding** of the complete world with an FNV-1a checksum.

Units: distances in micro-units (`MICROS_PER_UNIT` per table unit, for example
micrometres per metre), velocities in micro-units per second, ratios in
millionths. Bodies, walls and sensors are processed in insertion order; no
floating point affects state.

The crate depends on no renderer, transport, persistence or identity, and
holds no game rules: a game wraps a `World` with its scoring and inputs.
