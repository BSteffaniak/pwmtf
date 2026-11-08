# PWMTF eight-ball

Authoritative 8-ball rules on `pwmtf_billiards` physics: rack and break, group
assignment, fouls and ball-in-hand, the called 8-ball, timeouts, concession,
rematch breaker selection, and versioned canonical encodings. It depends on no
renderer, transport, persistence or identity, so any host can embed it.

## Rules profiles

Each match pins one immutable profile version:

- `RulesProfile::standard()` (version 1): PWMTF's rules. An 8-ball pocketed on
  the break re-racks, turns are timed at 30 seconds, and an uncalled 8-ball
  shot that pockets the 8 loses.
- `RulesProfile::lounge()` (version 2): casual house rules for Cozy Lounge. An
  8-ball pocketed on the break is re-spotted at the foot spot, a clean break
  that pockets any object ball keeps the turn, shooting at the 8 requires a
  called pocket (refused otherwise), a scratched cue ball is re-spotted at the
  head spot with ball-in-hand that can be adjusted until the shot, and turns
  are untimed.

Table geometry is independent of the profile; `TableGeometry::new` accepts any
table, ball and pocket size.

## Synchronization

`MatchState` implements `pwmtf_sync::ReplayGame`, so a host gets revision-checked,
idempotent authority and client replay with `pwmtf_sync::Authority<MatchState>`
and `pwmtf_sync::Replica<MatchState>`.
