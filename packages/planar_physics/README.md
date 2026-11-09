# PWMTF planar physics

Deterministic, integer fixed-point 2D physics for live table games such as
air hockey, foosball, bumper carts, marble races, pinball and paper-ball
basketball. A native authoritative server and WASM clients compute the same
bits, so states can be predicted, rolled back and verified by checksum (see
the tick mode of `pwmtf_sync`).

- **Dynamic discs** (pucks, balls, marbles) with mass, restitution, linear
  damping and constant friction deceleration.
- **Shaped bodies** (`World::add_shaped_body`): one to sixteen parts, each a
  disc, an oriented rectangle or a capsule (a segment thickened by a radius)
  at an offset and angle, with an angle, angular velocity and moment of
  inertia (derived from the parts' areas or given). Contacts between any two
  shapes (and against walls) resolve with rotation, restitution and Coulomb
  friction (the geometric mean of both surfaces' coefficients; walls default
  to frictionless, see `World::set_wall_friction`), so balls spin off angled
  surfaces and off-centre hits turn what they strike.
- **Kinematic rotation**: kinematic shaped bodies chase an angle target at a
  bounded angular speed as well as a position target, optionally within an
  angle range (a flipper between rest and raised). Rigid parts orbit the
  body's origin; spindle parts turn about their own centres, like figures on
  a foosball rod that also slides. Contacts use each surface point's velocity
  (linear plus angular), so a swinging flipper or spinning figure launches a
  ball faster than the point that struck it.
- **Steering drives** (`Drive`, `World::set_drive`): throttle accelerates
  along the body's local +x axis, steer sets a turn rate approached at a
  bounded angular acceleration, and lateral grip removes sideways sliding so
  bumper carts track their heading yet still slide briefly when bumped.
  Constant table friction also brakes spin, so idle bodies come exactly to
  rest.
- **Kinematic discs** driven toward a target at a bounded speed and held in a
  confinement rectangle: a pointer-driven mallet or rod sets its target each
  tick and pushes dynamic discs as if infinitely heavy.
- **Static wall segments** and rectangular arena bounds.
- **Acceleration fields**: a world-wide acceleration (`World::set_gravity`)
  and regional ones (`World::add_field`) for slopes and tilt, changeable
  every tick. Each tick adds exactly the per-tick share across its substeps,
  and contacts approaching slower than one tick of acceleration do not
  bounce, so resting bodies settle instead of chattering.
- **Bumpers and slingshots**: kinematic parts with a `kick` add a fixed
  speed along the contact normal when a dynamic body starts touching them,
  firing once per contact (with a small hysteresis) and reporting
  `Event::Bumped`.
- **Sensors** (rectangles or circles) that report a body's center entering or
  leaving exactly once, such as goals.
- **Layers and masks** to filter which bodies collide or are sensed.
- **Bounded substeps**: each tick is split so no surface point (linear plus
  angular speed times reach) moves more than half the thinnest part per
  substep; `WorldConfig::min_radius` rejects bodies whose thinnest part is
  too thin to be tunnelling-safe at the world's speed limit. Angular speed
  caps are lowered so rotation alone never moves a point faster than the
  world's limit; when linear plus tip speed can exceed it, raise
  `max_substeps` or the parts' thickness accordingly.
- **Canonical events**: contacts (merged per pair per tick, with approach
  speed for audio), part-level contact details with point and normal for
  shaped bodies, bumper kicks and sensor changes in a fixed order.
- **Versioned encoding** of the complete world with an FNV-1a checksum.
  Worlds of only plain discs, frictionless walls and no acceleration still
  encode as version 1, byte for byte as before, so existing checksums and
  stored states remain valid; anything else encodes as version 2. Decoding
  accepts both and rejects every other version.

Units: distances in micro-units (`MICROS_PER_UNIT` per table unit, for example
micrometres per metre), velocities in micro-units per second, ratios in
millionths, angles in microradians (counter-clockwise, wrapped by
`TURN_MICRORADIANS`) and angular velocities in microradians per second.
Sine and cosine come from an integer series (`sin_cos`), so rotation is
bit-identical everywhere. Bodies, walls and sensors are processed in insertion order; no
floating point affects state.

The crate depends on no renderer, transport, persistence or identity, and
holds no game rules: a game wraps a `World` with its scoring and inputs.

## Choosing shapes

- **Foosball**: a kinematic shaped body per rod with a rectangle per figure on
  a spindle (`Attachment::Spindle`), confined to slide along the rod's axis;
  drive the slide with `set_target` and the spin with `set_angle_target`. A
  figure's upper half sweeps toward +x when the spin turns clockwise.
  Alternatively keep figures rigid and add the kick as an x offset of the
  rod's target (the projection of the spin), deactivating the rod while the
  figures are spun over the ball.
- **Bumper carts**: dynamic capsules (a stadium is a good cart outline: cart
  against cart is exact capsule against capsule) with a `Drive`, restitution
  around 0.6–0.8, some constant friction and an angular speed cap of a few
  radians per second.
- **Marble race**: plain dynamic discs (or shaped discs with surface friction
  for spin) on walls, with `set_gravity` set from the player's tilt each tick
  and fields for boosters.
- **Pinball**: a plain dynamic disc ball, kinematic capsule flippers whose
  origin is the pivot (the capsule offset half its length) with an angle
  range from rest to raised, kicking discs for pop bumpers and kicking
  capsules for slingshots, and the playfield slope as gravity.
