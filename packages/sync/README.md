# PWMTF sync

Transport-free synchronization for deterministic games, shared by PWMTF and
Cozy Lounge. Two modes share one philosophy: the authority runs the same
deterministic game code as every client, so clients can predict and verify,
but only the authority's results count.

Hosts own identity (mapping players to seats), transport, framing,
persistence, presentation and the clock. Nothing here does I/O.

## Command replay (turn-based games)

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

Time is the host's millisecond clock.

## Tick-stamped inputs with rollback (real-time games)

A game implements [`TickGame`]: a seat type, a small quantized `Input`
(device-neutral intent such as a pointer target or axis values, never a key
press, so mouse, keyboard and touch share one representation), a neutral
input, a `step` that advances exactly one fixed tick from one input per
seated seat in seat order, membership hooks, a checksum and a versioned
encoding.

- [`TickAuthority`] owns the canonical state. The host calls `advance()` once
  per tick on its own clock; each call applies pending joins and leaves,
  steps with every seat's input and returns a [`TickFrame`] (inputs used,
  membership changes, and a checksum every `checksum_interval` ticks) to
  broadcast. `snapshot()` serves late joiners and resyncs; `frames_from()`
  retransmits from a bounded history.
- **Input window.** An input stamped for tick `t` is accepted when
  `current <= t < current + input_window`. Late inputs (for ticks already
  simulated) are dropped and refused; the seat's previous input repeats for
  that tick, which the frame records, so clients roll back to it. Inputs too
  far ahead are refused, bounding memory. The first input per seat and tick
  wins; identical retransmissions are idempotent duplicates and different
  ones are conflicts.
- [`TickReplica`] mirrors it on a client. A player stamps its input for the
  predicted tick and steps immediately, so its own input is visible at once,
  predicting remote seats with their newest confirmed input, up to
  `prediction_horizon` ticks ahead of confirmation. A confirmed frame that
  matches the prediction promotes the predicted state; one that differs rolls
  back to the confirmed state and re-simulates to the present, reporting a
  [`Correction`] (ticks re-simulated and the game's own measure of how far
  presentation jumped) so the host can smooth. Frames carrying checksums are
  verified; a mismatch stops integration and `request()` asks for a snapshot,
  which `adopt()` applies. Spectators never predict: they present confirmed
  ticks behind a small jitter buffer.
- Out-of-order and duplicated frames are buffered and deduplicated; a gap
  makes `request()` ask for frames from the first missing tick.

### Hosting guidance

- **Clock.** Estimate the authority's current tick (clock sync) and keep each
  player's predicted tick ahead of it by one-way latency + jitter + the input
  batching interval, plus one more batching interval so a single lost
  message is repaired by the next. [`lead_ticks`] gives a conservative
  ahead-of-confirmed figure from a round trip and jitter.
- **Inputs.** Send `unconfirmed_inputs()` in every input message (batched,
  for example every 50 ms, so a client stays well under 20–30 messages per
  second), together with the confirmed tick as an acknowledgement. Redundancy
  repairs loss; the authority deduplicates.
- **Frames.** Batch frames (for example three 60 Hz frames per message). Over
  a reliable ordered transport such as a WebSocket, send each frame once;
  over an unreliable one, send frames from each client's acknowledgement.
- **Snapshots.** Serve `snapshot()` to late joiners and whenever a replica's
  `request()` says so, rate limited.
