# PWMTF sync

Transport-free synchronization for deterministic games, shared by PWMTF and
Cozy Lounge.

A game implements [`ReplayGame`]: seats, commands, a deterministic `apply` that
reports how many fixed ticks of motion a command plays back, a checksum, an
optional turn duration and a playback type. On top of that:

- [`Authority`] accepts commands with revision checks and idempotency keys.
  `submit` computes a [`Proposal`] without mutating anything and `commit`
  publishes it, so a host that must persist before acknowledging (PWMTF)
  writes the proposal in between, while an in-memory host (Cozy Lounge)
  commits immediately. Turn deadlines start after a command's motion ends.
- [`Replica`] mirrors authority on a client: it predicts the local player's
  one pending command (deferring expensive resolution until after playback),
  retransmits it with the same key after a reconnect, recognizes its
  acceptance by key, replays every accepted command's motion from the
  previous state, and reports each finished transition for summaries.

Hosts own identity (mapping players to seats), transport, framing,
persistence and presentation. Time is the host's millisecond clock.

Continuous real-time games will add tick-stamped input synchronization on the
same deterministic contract; turn-based games use command replay today.
