# PWMTF billiards

Deterministic, renderer-independent billiards physics: table geometry, ball
state, bounded shot input (aim, power, spin), fixed-tick simulation with
collisions, cushions and pockets, canonically ordered events, and versioned
encodings. All arithmetic is integer fixed-point, so a native server and WASM
clients resolve the same shot to the same bits. Rules live in separate crates
such as `pwmtf_eight_ball`; the crate depends on no renderer, transport,
persistence or identity.
