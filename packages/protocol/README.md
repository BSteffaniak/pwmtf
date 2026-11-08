# PWMTF protocol

This package owns bounded, explicitly versioned wire representations. It validates framing and compatibility but delegates every gameplay decision to `pwmtf_billiards` and `pwmtf_eight_ball`.

`CommandEnvelope`, `SnapshotEnvelope`, `ShotPreview`, and `CommandId` keep their version-1 encodings; command envelope bytes are journaled and compared for idempotency. The `session` module defines session version 2 (`SESSION_VERSION`): binary, tag-prefixed `ClientMessage` and `ServerMessage` frames for the single `/ws` socket. Every decoder rejects unknown tags, truncation, trailing bytes, invalid flags or discriminants, and frames beyond `MAX_CLIENT_MESSAGE_BYTES` or `MAX_SERVER_MESSAGE_BYTES`. `MatchState` carries the receiving seat, deadline, server time, a complete snapshot, and an optional `LastCommand` (command, actor, `playback_ticks`) for deterministic observer replay. `Rejected` carries a typed `RejectReason`.
