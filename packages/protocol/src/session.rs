//! Version-two session message layer carried in binary WebSocket frames.
//!
//! One socket serves every match and lobby subscription of an account. The
//! first byte of every frame is a tag; integers are big-endian; an
//! `Option<u64>` is a `0`/`1` flag byte followed by the value when present.
//! Every decoder fails closed on unknown tags, truncation, trailing bytes,
//! invalid flags or discriminants, and oversize frames.

use pwmtf_eight_ball::{MATCH_COMMAND_VERSION, Player, VersionedMatchCommand};

use crate::{
    CommandEnvelope, CommandId, MAX_FRAME_BYTES, MAX_NEGOTIATED_VERSIONS, MAX_PREVIEW_FRAME_BYTES,
    MAX_SNAPSHOT_BYTES, ProtocolError, Reader, ShotPreview, decode_command, encode_command,
};

/// Session message-layer version negotiated by `Hello`/`Welcome`.
pub const SESSION_VERSION: u16 = 2;
/// Maximum encoded client-to-server session frame.
pub const MAX_CLIENT_MESSAGE_BYTES: usize = 512;
/// Maximum encoded server-to-client session frame.
pub const MAX_SERVER_MESSAGE_BYTES: usize = MAX_SNAPSHOT_BYTES + 256;

const TAG_HELLO: u8 = 0x01;
const TAG_SUBSCRIBE_MATCH: u8 = 0x02;
const TAG_UNSUBSCRIBE_MATCH: u8 = 0x03;
const TAG_SUBSCRIBE_LOBBY: u8 = 0x04;
const TAG_UNSUBSCRIBE_LOBBY: u8 = 0x05;
const TAG_COMMAND: u8 = 0x06;
const TAG_CLIENT_PREVIEW: u8 = 0x07;
const TAG_PING: u8 = 0x08;

const TAG_WELCOME: u8 = 0x81;
const TAG_MATCH_STATE: u8 = 0x82;
const TAG_REJECTED: u8 = 0x83;
const TAG_SERVER_PREVIEW: u8 = 0x84;
const TAG_PONG: u8 = 0x85;
const TAG_LOBBY_CHANGED: u8 = 0x86;
const TAG_MATCH_STARTED: u8 = 0x87;
const TAG_SOCIAL_CHANGED: u8 = 0x88;
const TAG_SUBSCRIPTION_DENIED: u8 = 0x89;

/// Client-to-server session message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientMessage {
    /// Mandatory first frame offering supported session versions.
    Hello {
        /// Offered versions; `1..=MAX_NEGOTIATED_VERSIONS` entries.
        versions: Vec<u16>,
    },
    /// Requests authoritative state for a match the account participates in.
    SubscribeMatch {
        /// Match identifier.
        match_id: u128,
    },
    /// Stops receiving state for a match.
    UnsubscribeMatch {
        /// Match identifier.
        match_id: u128,
    },
    /// Requests lobby change notifications; also establishes lobby presence.
    SubscribeLobby {
        /// Lobby identifier.
        lobby_id: u128,
    },
    /// Stops lobby notifications and presence.
    UnsubscribeLobby {
        /// Lobby identifier.
        lobby_id: u128,
    },
    /// Revision-bound, idempotent gameplay command for a subscribed match.
    Command {
        /// Match identifier.
        match_id: u128,
        /// Unchanged version-one command envelope.
        envelope: CommandEnvelope,
    },
    /// Ephemeral, non-authoritative aiming presentation.
    Preview {
        /// Match identifier.
        match_id: u128,
        /// Fixed-size preview payload.
        preview: ShotPreview,
    },
    /// Liveness and clock-synchronization probe.
    Ping {
        /// Opaque value echoed by `Pong`.
        nonce: u64,
    },
}

impl ClientMessage {
    /// Encodes the message into one bounded binary frame.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::EmptyVersionOffer`] or
    /// [`ProtocolError::TooManyVersions`] for an out-of-bounds `Hello` offer,
    /// or [`ProtocolError::FrameTooLarge`] if the encoded frame would exceed
    /// [`MAX_CLIENT_MESSAGE_BYTES`].
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut bytes = Vec::with_capacity(64);
        match self {
            Self::Hello { versions } => {
                let count = validate_version_count(versions.len())?;
                bytes.push(TAG_HELLO);
                bytes.push(count);
                for version in versions {
                    bytes.extend_from_slice(&version.to_be_bytes());
                }
            }
            Self::SubscribeMatch { match_id } => {
                push_id(&mut bytes, TAG_SUBSCRIBE_MATCH, *match_id);
            }
            Self::UnsubscribeMatch { match_id } => {
                push_id(&mut bytes, TAG_UNSUBSCRIBE_MATCH, *match_id);
            }
            Self::SubscribeLobby { lobby_id } => {
                push_id(&mut bytes, TAG_SUBSCRIBE_LOBBY, *lobby_id);
            }
            Self::UnsubscribeLobby { lobby_id } => {
                push_id(&mut bytes, TAG_UNSUBSCRIBE_LOBBY, *lobby_id);
            }
            Self::Command { match_id, envelope } => {
                push_id(&mut bytes, TAG_COMMAND, *match_id);
                let encoded = envelope.to_bytes();
                if encoded.len() > MAX_FRAME_BYTES {
                    return Err(ProtocolError::FrameTooLarge(encoded.len()));
                }
                let length = u16::try_from(encoded.len())
                    .map_err(|_| ProtocolError::FrameTooLarge(encoded.len()))?;
                bytes.extend_from_slice(&length.to_be_bytes());
                bytes.extend_from_slice(&encoded);
            }
            Self::Preview { match_id, preview } => {
                push_id(&mut bytes, TAG_CLIENT_PREVIEW, *match_id);
                bytes.extend_from_slice(&preview.to_bytes());
            }
            Self::Ping { nonce } => {
                bytes.push(TAG_PING);
                bytes.extend_from_slice(&nonce.to_be_bytes());
            }
        }
        bounded(bytes, MAX_CLIENT_MESSAGE_BYTES)
    }

    /// Decodes and validates one client frame.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FrameTooLarge`] above
    /// [`MAX_CLIENT_MESSAGE_BYTES`], [`ProtocolError::InvalidDiscriminant`]
    /// for an unknown tag, [`ProtocolError::Truncated`] or
    /// [`ProtocolError::TrailingBytes`] for a mis-sized frame,
    /// [`ProtocolError::EmptyVersionOffer`] or
    /// [`ProtocolError::TooManyVersions`] for an out-of-bounds `Hello`, and
    /// any [`CommandEnvelope::from_bytes`] or [`ShotPreview::from_bytes`]
    /// error for an invalid nested payload.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_CLIENT_MESSAGE_BYTES {
            return Err(ProtocolError::FrameTooLarge(bytes.len()));
        }
        let mut reader = Reader::new(bytes);
        let message = match reader.u8()? {
            TAG_HELLO => {
                let count = usize::from(reader.u8()?);
                validate_version_count(count)?;
                let mut versions = Vec::with_capacity(count);
                for _ in 0..count {
                    versions.push(reader.u16()?);
                }
                Self::Hello { versions }
            }
            TAG_SUBSCRIBE_MATCH => Self::SubscribeMatch {
                match_id: reader.u128()?,
            },
            TAG_UNSUBSCRIBE_MATCH => Self::UnsubscribeMatch {
                match_id: reader.u128()?,
            },
            TAG_SUBSCRIBE_LOBBY => Self::SubscribeLobby {
                lobby_id: reader.u128()?,
            },
            TAG_UNSUBSCRIBE_LOBBY => Self::UnsubscribeLobby {
                lobby_id: reader.u128()?,
            },
            TAG_COMMAND => {
                let match_id = reader.u128()?;
                let length = usize::from(reader.u16()?);
                if length > MAX_FRAME_BYTES {
                    return Err(ProtocolError::FrameTooLarge(length));
                }
                let envelope = CommandEnvelope::from_bytes(reader.bytes(length)?)?;
                Self::Command { match_id, envelope }
            }
            TAG_CLIENT_PREVIEW => Self::Preview {
                match_id: reader.u128()?,
                preview: ShotPreview::from_bytes(reader.bytes(MAX_PREVIEW_FRAME_BYTES)?)?,
            },
            TAG_PING => Self::Ping {
                nonce: reader.u64()?,
            },
            tag => return Err(ProtocolError::InvalidDiscriminant(tag)),
        };
        finish(&reader)?;
        Ok(message)
    }
}

/// Why the server refused a gameplay command.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RejectReason {
    /// The command could not be decoded or validated.
    Malformed,
    /// `expected_revision` did not match the canonical revision.
    StaleRevision,
    /// The sender is not the active player.
    WrongTurn,
    /// The match already has a result.
    MatchCompleted,
    /// The domain rejected the transition, such as an invalid placement.
    IllegalMove,
    /// The command id was reused with a different actor or payload.
    IdempotencyConflict,
    /// An internal or durability failure prevented acceptance.
    Unavailable,
    /// The connection has no active subscription to the match.
    NotSubscribed,
}

impl RejectReason {
    /// Every reason in wire order.
    pub const ALL: [Self; 8] = [
        Self::Malformed,
        Self::StaleRevision,
        Self::WrongTurn,
        Self::MatchCompleted,
        Self::IllegalMove,
        Self::IdempotencyConflict,
        Self::Unavailable,
        Self::NotSubscribed,
    ];

    /// Returns the stable wire code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Malformed => 1,
            Self::StaleRevision => 2,
            Self::WrongTurn => 3,
            Self::MatchCompleted => 4,
            Self::IllegalMove => 5,
            Self::IdempotencyConflict => 6,
            Self::Unavailable => 7,
            Self::NotSubscribed => 8,
        }
    }

    /// Parses a stable wire code.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidDiscriminant`] for an unknown code.
    pub const fn from_code(code: u8) -> Result<Self, ProtocolError> {
        Ok(match code {
            1 => Self::Malformed,
            2 => Self::StaleRevision,
            3 => Self::WrongTurn,
            4 => Self::MatchCompleted,
            5 => Self::IllegalMove,
            6 => Self::IdempotencyConflict,
            7 => Self::Unavailable,
            8 => Self::NotSubscribed,
            _ => return Err(ProtocolError::InvalidDiscriminant(code)),
        })
    }
}

/// Kind of subscription the server refused.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SubscriptionKind {
    /// Match subscription.
    Match,
    /// Lobby subscription.
    Lobby,
}

impl SubscriptionKind {
    /// Returns the stable wire code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Match => 1,
            Self::Lobby => 2,
        }
    }

    /// Parses a stable wire code.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidDiscriminant`] for an unknown code.
    pub const fn from_code(code: u8) -> Result<Self, ProtocolError> {
        match code {
            1 => Ok(Self::Match),
            2 => Ok(Self::Lobby),
            _ => Err(ProtocolError::InvalidDiscriminant(code)),
        }
    }
}

/// The accepted transition from `revision - 1` to `revision`, sent only on
/// the broadcast immediately following an accepted command or timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LastCommand {
    /// Idempotency key of the accepted command.
    pub command_id: CommandId,
    /// Player who issued the command, or who timed out.
    pub actor: Player,
    /// Versioned canonical command.
    pub command: VersionedMatchCommand,
    /// Fixed physics ticks of shot playback; zero for non-shot commands.
    pub playback_ticks: u32,
}

/// Authoritative match state addressed to one participant's connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchStateMessage {
    /// Match identifier.
    pub match_id: u128,
    /// The receiving account's seat.
    pub seat: Player,
    /// Canonical match revision.
    pub revision: u64,
    /// Canonical state checksum.
    pub checksum: u64,
    /// Current turn deadline in Unix milliseconds, if one is running.
    pub deadline_at_ms: Option<u64>,
    /// Server clock in Unix milliseconds when the message was produced.
    pub server_now_ms: u64,
    /// The transition that produced this revision, if this is that broadcast.
    pub last_command: Option<LastCommand>,
    /// Complete canonical `MatchState::to_bytes()` snapshot.
    pub snapshot: Vec<u8>,
}

/// Server-to-client session message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerMessage {
    /// Handshake reply carrying the negotiated version.
    Welcome {
        /// Negotiated session version.
        version: u16,
        /// Server clock in Unix milliseconds.
        server_now_ms: u64,
    },
    /// Authoritative match state.
    MatchState(MatchStateMessage),
    /// A command was refused; a fresh `MatchState` follows unless the reason
    /// is [`RejectReason::NotSubscribed`].
    Rejected {
        /// Match identifier.
        match_id: u128,
        /// Refused command's idempotency key.
        command_id: CommandId,
        /// Refusal reason.
        reason: RejectReason,
    },
    /// The opponent's ephemeral aiming presentation.
    Preview {
        /// Match identifier.
        match_id: u128,
        /// Fixed-size preview payload.
        preview: ShotPreview,
    },
    /// Reply to `Ping`.
    Pong {
        /// Echoed nonce.
        nonce: u64,
        /// Server clock in Unix milliseconds.
        server_now_ms: u64,
    },
    /// The lobby changed; the client refetches it.
    LobbyChanged {
        /// Lobby identifier.
        lobby_id: u128,
    },
    /// A lobby atomically started a match.
    MatchStarted {
        /// Lobby identifier.
        lobby_id: u128,
        /// New match identifier.
        match_id: u128,
    },
    /// Challenges, lobbies or rematches changed; the client refetches lists.
    SocialChanged,
    /// A subscription was not authorized.
    SubscriptionDenied {
        /// Subscription kind.
        kind: SubscriptionKind,
        /// Match or lobby identifier.
        id: u128,
    },
}

impl ServerMessage {
    /// Encodes the message into one bounded binary frame.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FrameTooLarge`] if a snapshot exceeds
    /// [`MAX_SNAPSHOT_BYTES`] or the frame would exceed
    /// [`MAX_SERVER_MESSAGE_BYTES`].
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut bytes = Vec::with_capacity(64);
        match self {
            Self::Welcome {
                version,
                server_now_ms,
            } => {
                bytes.push(TAG_WELCOME);
                bytes.extend_from_slice(&version.to_be_bytes());
                bytes.extend_from_slice(&server_now_ms.to_be_bytes());
            }
            Self::MatchState(state) => encode_match_state(state, &mut bytes)?,
            Self::Rejected {
                match_id,
                command_id,
                reason,
            } => {
                push_id(&mut bytes, TAG_REJECTED, *match_id);
                bytes.extend_from_slice(&command_id.bytes());
                bytes.push(reason.code());
            }
            Self::Preview { match_id, preview } => {
                push_id(&mut bytes, TAG_SERVER_PREVIEW, *match_id);
                bytes.extend_from_slice(&preview.to_bytes());
            }
            Self::Pong {
                nonce,
                server_now_ms,
            } => {
                bytes.push(TAG_PONG);
                bytes.extend_from_slice(&nonce.to_be_bytes());
                bytes.extend_from_slice(&server_now_ms.to_be_bytes());
            }
            Self::LobbyChanged { lobby_id } => push_id(&mut bytes, TAG_LOBBY_CHANGED, *lobby_id),
            Self::MatchStarted { lobby_id, match_id } => {
                push_id(&mut bytes, TAG_MATCH_STARTED, *lobby_id);
                bytes.extend_from_slice(&match_id.to_be_bytes());
            }
            Self::SocialChanged => bytes.push(TAG_SOCIAL_CHANGED),
            Self::SubscriptionDenied { kind, id } => {
                bytes.push(TAG_SUBSCRIPTION_DENIED);
                bytes.push(kind.code());
                bytes.extend_from_slice(&id.to_be_bytes());
            }
        }
        bounded(bytes, MAX_SERVER_MESSAGE_BYTES)
    }

    /// Decodes and validates one server frame.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FrameTooLarge`] above
    /// [`MAX_SERVER_MESSAGE_BYTES`] or for a snapshot above
    /// [`MAX_SNAPSHOT_BYTES`], [`ProtocolError::InvalidDiscriminant`] for an
    /// unknown tag, seat, actor, option flag, reason or subscription kind,
    /// [`ProtocolError::UnsupportedCommand`] for an unknown last-command
    /// version, [`ProtocolError::Truncated`] or
    /// [`ProtocolError::TrailingBytes`] for a mis-sized frame, and any
    /// command or [`ShotPreview::from_bytes`] validation error for an invalid
    /// nested payload.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_SERVER_MESSAGE_BYTES {
            return Err(ProtocolError::FrameTooLarge(bytes.len()));
        }
        let mut reader = Reader::new(bytes);
        let message = match reader.u8()? {
            TAG_WELCOME => Self::Welcome {
                version: reader.u16()?,
                server_now_ms: reader.u64()?,
            },
            TAG_MATCH_STATE => Self::MatchState(decode_match_state(&mut reader)?),
            TAG_REJECTED => Self::Rejected {
                match_id: reader.u128()?,
                command_id: CommandId::new(reader.array()?),
                reason: RejectReason::from_code(reader.u8()?)?,
            },
            TAG_SERVER_PREVIEW => Self::Preview {
                match_id: reader.u128()?,
                preview: ShotPreview::from_bytes(reader.bytes(MAX_PREVIEW_FRAME_BYTES)?)?,
            },
            TAG_PONG => Self::Pong {
                nonce: reader.u64()?,
                server_now_ms: reader.u64()?,
            },
            TAG_LOBBY_CHANGED => Self::LobbyChanged {
                lobby_id: reader.u128()?,
            },
            TAG_MATCH_STARTED => Self::MatchStarted {
                lobby_id: reader.u128()?,
                match_id: reader.u128()?,
            },
            TAG_SOCIAL_CHANGED => Self::SocialChanged,
            TAG_SUBSCRIPTION_DENIED => Self::SubscriptionDenied {
                kind: SubscriptionKind::from_code(reader.u8()?)?,
                id: reader.u128()?,
            },
            tag => return Err(ProtocolError::InvalidDiscriminant(tag)),
        };
        finish(&reader)?;
        Ok(message)
    }
}

fn encode_match_state(state: &MatchStateMessage, bytes: &mut Vec<u8>) -> Result<(), ProtocolError> {
    if state.snapshot.len() > MAX_SNAPSHOT_BYTES {
        return Err(ProtocolError::FrameTooLarge(state.snapshot.len()));
    }
    push_id(bytes, TAG_MATCH_STATE, state.match_id);
    bytes.push(encode_player(state.seat));
    bytes.extend_from_slice(&state.revision.to_be_bytes());
    bytes.extend_from_slice(&state.checksum.to_be_bytes());
    match state.deadline_at_ms {
        None => bytes.push(0),
        Some(deadline) => {
            bytes.push(1);
            bytes.extend_from_slice(&deadline.to_be_bytes());
        }
    }
    bytes.extend_from_slice(&state.server_now_ms.to_be_bytes());
    match state.last_command {
        None => bytes.push(0),
        Some(last) => {
            bytes.push(1);
            bytes.extend_from_slice(&last.command_id.bytes());
            bytes.push(encode_player(last.actor));
            bytes.extend_from_slice(&last.command.version.to_be_bytes());
            encode_command(last.command.command, bytes);
            bytes.extend_from_slice(&last.playback_ticks.to_be_bytes());
        }
    }
    let length = u32::try_from(state.snapshot.len())
        .map_err(|_| ProtocolError::FrameTooLarge(state.snapshot.len()))?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(&state.snapshot);
    Ok(())
}

fn decode_match_state(reader: &mut Reader<'_>) -> Result<MatchStateMessage, ProtocolError> {
    let match_id = reader.u128()?;
    let seat = decode_player(reader.u8()?)?;
    let revision = reader.u64()?;
    let checksum = reader.u64()?;
    let deadline_at_ms = if decode_flag(reader.u8()?)? {
        Some(reader.u64()?)
    } else {
        None
    };
    let server_now_ms = reader.u64()?;
    let last_command = if decode_flag(reader.u8()?)? {
        let command_id = CommandId::new(reader.array()?);
        let actor = decode_player(reader.u8()?)?;
        let version = reader.u16()?;
        if version != MATCH_COMMAND_VERSION {
            return Err(ProtocolError::UnsupportedCommand(version));
        }
        let command = decode_command(reader)?;
        Some(LastCommand {
            command_id,
            actor,
            command: VersionedMatchCommand { version, command },
            playback_ticks: reader.u32()?,
        })
    } else {
        None
    };
    let size = usize::try_from(reader.u32()?).map_err(|_| ProtocolError::Truncated)?;
    if size > MAX_SNAPSHOT_BYTES {
        return Err(ProtocolError::FrameTooLarge(size));
    }
    let snapshot = reader.bytes(size)?.to_vec();
    Ok(MatchStateMessage {
        match_id,
        seat,
        revision,
        checksum,
        deadline_at_ms,
        server_now_ms,
        last_command,
        snapshot,
    })
}

fn validate_version_count(count: usize) -> Result<u8, ProtocolError> {
    if count == 0 {
        return Err(ProtocolError::EmptyVersionOffer);
    }
    if count > MAX_NEGOTIATED_VERSIONS {
        return Err(ProtocolError::TooManyVersions);
    }
    u8::try_from(count).map_err(|_| ProtocolError::TooManyVersions)
}

fn push_id(bytes: &mut Vec<u8>, tag: u8, id: u128) {
    bytes.push(tag);
    bytes.extend_from_slice(&id.to_be_bytes());
}

fn bounded(bytes: Vec<u8>, maximum: usize) -> Result<Vec<u8>, ProtocolError> {
    if bytes.len() > maximum {
        Err(ProtocolError::FrameTooLarge(bytes.len()))
    } else {
        Ok(bytes)
    }
}

const fn finish(reader: &Reader<'_>) -> Result<(), ProtocolError> {
    if reader.finished() {
        Ok(())
    } else {
        Err(ProtocolError::TrailingBytes(reader.remaining()))
    }
}

const fn encode_player(player: Player) -> u8 {
    match player {
        Player::One => 1,
        Player::Two => 2,
    }
}

const fn decode_player(value: u8) -> Result<Player, ProtocolError> {
    match value {
        1 => Ok(Player::One),
        2 => Ok(Player::Two),
        _ => Err(ProtocolError::InvalidDiscriminant(value)),
    }
}

const fn decode_flag(value: u8) -> Result<bool, ProtocolError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ProtocolError::InvalidDiscriminant(value)),
    }
}

#[cfg(test)]
mod tests {
    use pwmtf_billiards::{Aim, PocketId, ShotPower, Spin, Vector, VersionedShotCommand};
    use pwmtf_eight_ball::MatchCommand;

    use super::*;

    fn preview() -> ShotPreview {
        ShotPreview {
            revision: 3,
            gesture: 11,
            sequence: 5,
            aim: Aim::new(900).unwrap(),
            power: ShotPower::new(1_234).unwrap(),
            spin: Spin::new(-50, 75).unwrap(),
            cancel: false,
        }
    }

    fn shot() -> VersionedMatchCommand {
        VersionedMatchCommand::new(MatchCommand::PlayShot {
            shot: VersionedShotCommand::new(
                Aim::new(123).unwrap(),
                ShotPower::new(4_567).unwrap(),
                Spin::new(-200, 300).unwrap(),
            ),
            called_pocket: Some(PocketId::TopCenter),
        })
    }

    const MATCH_ID: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;

    fn client_messages() -> Vec<ClientMessage> {
        let mut messages = vec![
            ClientMessage::Hello {
                versions: vec![SESSION_VERSION],
            },
            ClientMessage::Hello {
                versions: vec![u16::MAX; MAX_NEGOTIATED_VERSIONS],
            },
            ClientMessage::SubscribeMatch { match_id: MATCH_ID },
            ClientMessage::UnsubscribeMatch {
                match_id: u128::MAX,
            },
            ClientMessage::SubscribeLobby { lobby_id: 7 },
            ClientMessage::UnsubscribeLobby { lobby_id: 0 },
            ClientMessage::Preview {
                match_id: MATCH_ID,
                preview: preview(),
            },
            ClientMessage::Ping { nonce: u64::MAX },
        ];
        let commands = [
            shot(),
            VersionedMatchCommand::new(MatchCommand::Timeout),
            VersionedMatchCommand::new(MatchCommand::Concede {
                player: Player::Two,
            }),
            VersionedMatchCommand::new(MatchCommand::PlaceCueBall {
                position: Vector::from_micros(-123, 456),
            }),
        ];
        for command in commands {
            messages.push(ClientMessage::Command {
                match_id: MATCH_ID,
                envelope: CommandEnvelope::new(9, CommandId::new([4; 16]), command),
            });
        }
        messages
    }

    fn match_state(deadline: Option<u64>, last_command: Option<LastCommand>) -> MatchStateMessage {
        MatchStateMessage {
            match_id: MATCH_ID,
            seat: Player::Two,
            revision: 12,
            checksum: 0xdead_beef,
            deadline_at_ms: deadline,
            server_now_ms: 1_700_000_000_000,
            last_command,
            snapshot: vec![1, 2, 3, 4, 5],
        }
    }

    fn last_command(command: VersionedMatchCommand) -> LastCommand {
        LastCommand {
            command_id: CommandId::new([9; 16]),
            actor: Player::One,
            command,
            playback_ticks: 1_234,
        }
    }

    fn server_messages() -> Vec<ServerMessage> {
        let mut messages = vec![
            ServerMessage::Welcome {
                version: SESSION_VERSION,
                server_now_ms: 99,
            },
            ServerMessage::MatchState(match_state(None, None)),
            ServerMessage::MatchState(match_state(
                Some(1_700_000_030_000),
                Some(last_command(shot())),
            )),
            ServerMessage::MatchState(MatchStateMessage {
                snapshot: vec![0xab; MAX_SNAPSHOT_BYTES],
                ..match_state(Some(0), Some(last_command(shot())))
            }),
            ServerMessage::MatchState(MatchStateMessage {
                snapshot: Vec::new(),
                ..match_state(
                    None,
                    Some(LastCommand {
                        actor: Player::Two,
                        playback_ticks: 0,
                        ..last_command(VersionedMatchCommand::new(MatchCommand::Timeout))
                    }),
                )
            }),
            ServerMessage::Preview {
                match_id: MATCH_ID,
                preview: ShotPreview {
                    cancel: true,
                    ..preview()
                },
            },
            ServerMessage::Pong {
                nonce: 5,
                server_now_ms: 6,
            },
            ServerMessage::LobbyChanged { lobby_id: 77 },
            ServerMessage::MatchStarted {
                lobby_id: 77,
                match_id: MATCH_ID,
            },
            ServerMessage::SocialChanged,
            ServerMessage::SubscriptionDenied {
                kind: SubscriptionKind::Match,
                id: MATCH_ID,
            },
            ServerMessage::SubscriptionDenied {
                kind: SubscriptionKind::Lobby,
                id: 1,
            },
        ];
        for reason in RejectReason::ALL {
            messages.push(ServerMessage::Rejected {
                match_id: MATCH_ID,
                command_id: CommandId::new([3; 16]),
                reason,
            });
        }
        messages
    }

    #[test]
    fn every_client_message_round_trips_and_fails_closed_on_truncation_and_trailing_bytes() {
        let messages = client_messages();
        let mut tags = std::collections::BTreeSet::new();
        for message in messages {
            let bytes = message.to_bytes().unwrap();
            assert!(bytes.len() <= MAX_CLIENT_MESSAGE_BYTES);
            tags.insert(bytes[0]);
            assert_eq!(ClientMessage::from_bytes(&bytes), Ok(message.clone()));
            for length in 0..bytes.len() {
                assert!(
                    ClientMessage::from_bytes(&bytes[..length]).is_err(),
                    "{message:?} truncated to {length} bytes decoded"
                );
            }
            let mut trailing = bytes;
            trailing.push(0);
            assert_eq!(
                ClientMessage::from_bytes(&trailing),
                Err(ProtocolError::TrailingBytes(1))
            );
        }
        assert_eq!(tags, (0x01..=0x08).collect());
    }

    #[test]
    fn every_server_message_round_trips_and_fails_closed_on_truncation_and_trailing_bytes() {
        let messages = server_messages();
        let mut tags = std::collections::BTreeSet::new();
        for message in messages {
            let bytes = message.to_bytes().unwrap();
            assert!(bytes.len() <= MAX_SERVER_MESSAGE_BYTES);
            tags.insert(bytes[0]);
            assert_eq!(ServerMessage::from_bytes(&bytes), Ok(message.clone()));
            for length in 0..bytes.len() {
                assert!(
                    ServerMessage::from_bytes(&bytes[..length]).is_err(),
                    "{message:?} truncated to {length} bytes decoded"
                );
            }
            let mut trailing = bytes;
            trailing.push(0);
            assert_eq!(
                ServerMessage::from_bytes(&trailing),
                Err(ProtocolError::TrailingBytes(1))
            );
        }
        assert_eq!(tags, (0x81..=0x89).collect());
    }

    #[test]
    fn match_state_wire_layout_follows_the_contract_field_order() {
        let message = ServerMessage::MatchState(match_state(
            Some(0x1122),
            Some(last_command(VersionedMatchCommand::new(
                MatchCommand::Timeout,
            ))),
        ));
        let bytes = message.to_bytes().unwrap();
        let mut expected = vec![TAG_MATCH_STATE];
        expected.extend_from_slice(&MATCH_ID.to_be_bytes());
        expected.push(2);
        expected.extend_from_slice(&12_u64.to_be_bytes());
        expected.extend_from_slice(&0xdead_beef_u64.to_be_bytes());
        expected.push(1);
        expected.extend_from_slice(&0x1122_u64.to_be_bytes());
        expected.extend_from_slice(&1_700_000_000_000_u64.to_be_bytes());
        expected.push(1);
        expected.extend_from_slice(&[9; 16]);
        expected.push(1);
        expected.extend_from_slice(&MATCH_COMMAND_VERSION.to_be_bytes());
        expected.push(2);
        expected.extend_from_slice(&1_234_u32.to_be_bytes());
        expected.extend_from_slice(&5_u32.to_be_bytes());
        expected.extend_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn command_payload_reuses_the_unchanged_envelope_encoding() {
        let envelope = CommandEnvelope::new(9, CommandId::new([4; 16]), shot());
        let bytes = ClientMessage::Command {
            match_id: MATCH_ID,
            envelope,
        }
        .to_bytes()
        .unwrap();
        let encoded = envelope.to_bytes();
        assert_eq!(
            &bytes[17..19],
            &u16::try_from(encoded.len()).unwrap().to_be_bytes()
        );
        assert_eq!(&bytes[19..], encoded.as_slice());
    }

    #[test]
    fn unknown_tags_fail_closed_in_both_directions() {
        for tag in 0..=u8::MAX {
            if !(0x01..=0x08).contains(&tag) {
                assert_eq!(
                    ClientMessage::from_bytes(&[tag]),
                    Err(ProtocolError::InvalidDiscriminant(tag))
                );
            }
            if !(0x81..=0x89).contains(&tag) {
                assert_eq!(
                    ServerMessage::from_bytes(&[tag]),
                    Err(ProtocolError::InvalidDiscriminant(tag))
                );
            }
        }
        assert_eq!(
            ClientMessage::from_bytes(&[]),
            Err(ProtocolError::Truncated)
        );
        assert_eq!(
            ServerMessage::from_bytes(&[]),
            Err(ProtocolError::Truncated)
        );
    }

    #[test]
    fn oversize_frames_fail_closed() {
        assert_eq!(
            ClientMessage::from_bytes(&[TAG_PING; MAX_CLIENT_MESSAGE_BYTES + 1]),
            Err(ProtocolError::FrameTooLarge(MAX_CLIENT_MESSAGE_BYTES + 1))
        );
        assert_eq!(
            ServerMessage::from_bytes(&vec![TAG_PONG; MAX_SERVER_MESSAGE_BYTES + 1]),
            Err(ProtocolError::FrameTooLarge(MAX_SERVER_MESSAGE_BYTES + 1))
        );
        let oversized = ServerMessage::MatchState(MatchStateMessage {
            snapshot: vec![0; MAX_SNAPSHOT_BYTES + 1],
            ..match_state(None, None)
        });
        assert_eq!(
            oversized.to_bytes(),
            Err(ProtocolError::FrameTooLarge(MAX_SNAPSHOT_BYTES + 1))
        );
        // A declared snapshot length above the bound is rejected before reading.
        let mut bytes = ServerMessage::MatchState(match_state(None, None))
            .to_bytes()
            .unwrap();
        let length_at = bytes.len() - 5 - 4;
        bytes[length_at..length_at + 4]
            .copy_from_slice(&u32::try_from(MAX_SNAPSHOT_BYTES + 1).unwrap().to_be_bytes());
        assert_eq!(
            ServerMessage::from_bytes(&bytes),
            Err(ProtocolError::FrameTooLarge(MAX_SNAPSHOT_BYTES + 1))
        );
        // A declared nested envelope length above the bound is rejected.
        let mut command = vec![TAG_COMMAND];
        command.extend_from_slice(&MATCH_ID.to_be_bytes());
        command.extend_from_slice(&u16::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes());
        command.extend_from_slice(&[0; MAX_FRAME_BYTES + 1]);
        assert_eq!(
            ClientMessage::from_bytes(&command),
            Err(ProtocolError::FrameTooLarge(MAX_FRAME_BYTES + 1))
        );
    }

    #[test]
    fn hello_version_offers_are_bounded() {
        assert_eq!(
            ClientMessage::Hello { versions: vec![] }.to_bytes(),
            Err(ProtocolError::EmptyVersionOffer)
        );
        assert_eq!(
            ClientMessage::Hello {
                versions: vec![2; MAX_NEGOTIATED_VERSIONS + 1]
            }
            .to_bytes(),
            Err(ProtocolError::TooManyVersions)
        );
        assert_eq!(
            ClientMessage::from_bytes(&[TAG_HELLO, 0]),
            Err(ProtocolError::EmptyVersionOffer)
        );
        let mut too_many = vec![
            TAG_HELLO,
            u8::try_from(MAX_NEGOTIATED_VERSIONS + 1).unwrap(),
        ];
        too_many.extend(std::iter::repeat_n(0, 2 * (MAX_NEGOTIATED_VERSIONS + 1)));
        assert_eq!(
            ClientMessage::from_bytes(&too_many),
            Err(ProtocolError::TooManyVersions)
        );
    }

    #[test]
    fn invalid_flags_and_discriminants_fail_closed() {
        let bytes = ServerMessage::MatchState(match_state(Some(1), Some(last_command(shot()))))
            .to_bytes()
            .unwrap();
        let seat_at = 17;
        let deadline_flag_at = seat_at + 1 + 16;
        let last_flag_at = deadline_flag_at + 9 + 8;
        let actor_at = last_flag_at + 1 + 16;
        let version_at = actor_at + 1;
        let command_tag_at = version_at + 2;
        for (offset, value, expected) in [
            (seat_at, 0, ProtocolError::InvalidDiscriminant(0)),
            (seat_at, 3, ProtocolError::InvalidDiscriminant(3)),
            (deadline_flag_at, 2, ProtocolError::InvalidDiscriminant(2)),
            (last_flag_at, 2, ProtocolError::InvalidDiscriminant(2)),
            (actor_at, 0, ProtocolError::InvalidDiscriminant(0)),
            (version_at + 1, 2, ProtocolError::UnsupportedCommand(2)),
            (command_tag_at, 9, ProtocolError::InvalidDiscriminant(9)),
        ] {
            let mut corrupt = bytes.clone();
            corrupt[offset] = value;
            assert_eq!(ServerMessage::from_bytes(&corrupt), Err(expected));
        }
        // An invalid shot inside the last command is rejected by domain validation.
        let mut corrupt = bytes;
        corrupt[command_tag_at + 3..command_tag_at + 5].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            ServerMessage::from_bytes(&corrupt),
            Err(ProtocolError::InvalidAim(u16::MAX))
        );

        let rejected = ServerMessage::Rejected {
            match_id: 1,
            command_id: CommandId::new([0; 16]),
            reason: RejectReason::Malformed,
        }
        .to_bytes()
        .unwrap();
        for code in [0, 9, u8::MAX] {
            let mut corrupt = rejected.clone();
            *corrupt.last_mut().unwrap() = code;
            assert_eq!(
                ServerMessage::from_bytes(&corrupt),
                Err(ProtocolError::InvalidDiscriminant(code))
            );
        }
        let denied = ServerMessage::SubscriptionDenied {
            kind: SubscriptionKind::Lobby,
            id: 1,
        }
        .to_bytes()
        .unwrap();
        for code in [0, 3] {
            let mut corrupt = denied.clone();
            corrupt[1] = code;
            assert_eq!(
                ServerMessage::from_bytes(&corrupt),
                Err(ProtocolError::InvalidDiscriminant(code))
            );
        }
        for reason in RejectReason::ALL {
            assert_eq!(RejectReason::from_code(reason.code()), Ok(reason));
        }
    }

    #[test]
    fn invalid_nested_envelopes_and_previews_fail_closed() {
        let envelope = CommandEnvelope::new(9, CommandId::new([4; 16]), shot());
        let bytes = ClientMessage::Command {
            match_id: MATCH_ID,
            envelope,
        }
        .to_bytes()
        .unwrap();
        // Inner protocol version must remain version one.
        let mut corrupt = bytes.clone();
        corrupt[20] = 2;
        assert_eq!(
            ClientMessage::from_bytes(&corrupt),
            Err(ProtocolError::UnsupportedProtocol(2))
        );
        // A shorter declared length truncates the nested envelope.
        let mut corrupt = bytes.clone();
        corrupt[18] -= 1;
        assert!(ClientMessage::from_bytes(&corrupt).is_err());
        // A longer declared length runs past the frame.
        let mut corrupt = bytes;
        corrupt[18] += 1;
        assert_eq!(
            ClientMessage::from_bytes(&corrupt),
            Err(ProtocolError::Truncated)
        );

        let preview_bytes = ClientMessage::Preview {
            match_id: MATCH_ID,
            preview: preview(),
        }
        .to_bytes()
        .unwrap();
        let server_preview = ServerMessage::Preview {
            match_id: MATCH_ID,
            preview: preview(),
        }
        .to_bytes()
        .unwrap();
        for (marker_offset, value) in [(17, 0), (19, 9), (44, 2)] {
            let mut corrupt = preview_bytes.clone();
            corrupt[marker_offset] = value;
            assert!(ClientMessage::from_bytes(&corrupt).is_err());
            let mut corrupt = server_preview.clone();
            corrupt[marker_offset] = value;
            assert!(ServerMessage::from_bytes(&corrupt).is_err());
        }
    }
}
