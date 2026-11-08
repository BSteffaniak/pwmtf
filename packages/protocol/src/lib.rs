#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
//! Bounded, explicitly versioned PWMTF wire representations.

use pwmtf_billiards::{Aim, PocketId, ShotPower, Spin, Vector, VersionedShotCommand};
use pwmtf_eight_ball::{MatchCommand, VersionedMatchCommand};
use thiserror::Error;

mod session;

pub use session::{
    ClientMessage, LastCommand, MAX_CLIENT_MESSAGE_BYTES, MAX_SERVER_MESSAGE_BYTES,
    MatchStateMessage, RejectReason, SESSION_VERSION, ServerMessage, SubscriptionKind,
};

/// Current transport protocol version.
pub const PROTOCOL_VERSION: u16 = 1;
/// Ephemeral preview payload version, independent of durable command negotiation.
pub const PREVIEW_VERSION: u16 = 2;
/// Ephemeral shot-planning frame marker, disjoint from version-one commands and snapshots.
pub const PREVIEW_MARKER: u8 = 0xff;
/// Fixed maximum preview frame size.
pub const MAX_PREVIEW_FRAME_BYTES: usize = 32;

/// Temporary, non-authoritative aiming/power presentation. Never a match command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShotPreview {
    /// Canonical match revision for which this planning gesture is valid.
    pub revision: u64,
    /// Fresh random gesture identity; prevents a prior gesture from reappearing.
    pub gesture: u64,
    /// Monotonic update number within one match revision (also spans gestures).
    pub sequence: u32,
    /// Canonically quantized aim (ignored on cancel).
    pub aim: Aim,
    /// Canonically quantized power (ignored on cancel).
    pub power: ShotPower,
    /// Canonically quantized cue-ball contact point for the remote exact guide.
    pub spin: Spin,
    /// Whether to clear the previous preview rather than show this one.
    pub cancel: bool,
}

impl ShotPreview {
    /// Encodes the fixed-size, versioned preview independently of durable commands.
    #[must_use]
    pub fn to_bytes(self) -> [u8; MAX_PREVIEW_FRAME_BYTES] {
        let mut bytes = [0_u8; MAX_PREVIEW_FRAME_BYTES];
        bytes[0] = PREVIEW_MARKER;
        bytes[1..3].copy_from_slice(&PREVIEW_VERSION.to_be_bytes());
        bytes[3..11].copy_from_slice(&self.revision.to_be_bytes());
        bytes[11..19].copy_from_slice(&self.gesture.to_be_bytes());
        bytes[19..23].copy_from_slice(&self.sequence.to_be_bytes());
        bytes[23..25].copy_from_slice(&self.aim.steps().to_be_bytes());
        bytes[25..27].copy_from_slice(&self.power.units().to_be_bytes());
        bytes[27] = u8::from(self.cancel);
        bytes[28..30].copy_from_slice(&self.spin.side.to_be_bytes());
        bytes[30..32].copy_from_slice(&self.spin.vertical.to_be_bytes());
        bytes
    }

    /// Decodes a bounded preview; rejects versions, malformed values and reserved bytes.
    ///
    /// # Errors
    /// Returns [`ProtocolError`] for malformed, unsupported or out-of-range frames.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() != MAX_PREVIEW_FRAME_BYTES || bytes.first() != Some(&PREVIEW_MARKER) {
            return Err(ProtocolError::InvalidPreview);
        }
        let version = u16::from_be_bytes([bytes[1], bytes[2]]);
        if version != PREVIEW_VERSION {
            return Err(ProtocolError::UnsupportedProtocol(version));
        }
        if bytes[27] > 1 {
            return Err(ProtocolError::InvalidPreview);
        }
        Ok(Self {
            revision: u64::from_be_bytes(
                bytes[3..11]
                    .try_into()
                    .map_err(|_| ProtocolError::InvalidPreview)?,
            ),
            gesture: u64::from_be_bytes(
                bytes[11..19]
                    .try_into()
                    .map_err(|_| ProtocolError::InvalidPreview)?,
            ),
            sequence: u32::from_be_bytes(
                bytes[19..23]
                    .try_into()
                    .map_err(|_| ProtocolError::InvalidPreview)?,
            ),
            aim: Aim::new(u16::from_be_bytes([bytes[23], bytes[24]]))
                .map_err(|_| ProtocolError::InvalidPreview)?,
            power: ShotPower::new(u16::from_be_bytes([bytes[25], bytes[26]]))
                .map_err(|_| ProtocolError::InvalidPreview)?,
            spin: Spin::new(
                i16::from_be_bytes([bytes[28], bytes[29]]),
                i16::from_be_bytes([bytes[30], bytes[31]]),
            )
            .map_err(|_| ProtocolError::InvalidPreview)?,
            cancel: bytes[27] == 1,
        })
    }
}

/// Maximum number of protocol versions offered during negotiation.
pub const MAX_NEGOTIATED_VERSIONS: usize = 8;
/// Maximum accepted wire frame size.
pub const MAX_FRAME_BYTES: usize = 256;

/// Opaque, client-generated idempotency key.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CommandId([u8; 16]);

impl CommandId {
    /// Creates a command identifier from exact opaque bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the exact opaque bytes.
    #[must_use]
    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Revision-bound state-changing command sent to the authoritative server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandEnvelope {
    /// Explicit protocol version.
    pub protocol_version: u16,
    /// Match revision the command was created against.
    pub expected_revision: u64,
    /// Idempotency key.
    pub command_id: CommandId,
    /// Versioned canonical domain command.
    pub command: VersionedMatchCommand,
}

impl CommandEnvelope {
    /// Creates a current-version envelope.
    #[must_use]
    pub const fn new(
        expected_revision: u64,
        command_id: CommandId,
        command: VersionedMatchCommand,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            expected_revision,
            command_id,
            command,
        }
    }

    /// Encodes the envelope into the stable version-one wire format.
    #[must_use]
    pub fn to_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(&self.protocol_version.to_be_bytes());
        bytes.extend_from_slice(&self.expected_revision.to_be_bytes());
        bytes.extend_from_slice(&self.command_id.bytes());
        bytes.extend_from_slice(&self.command.version.to_be_bytes());
        encode_command(self.command.command, &mut bytes);
        bytes
    }

    /// Decodes and validates a bounded command frame.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] for oversized, truncated, trailing,
    /// unsupported, invalid-discriminant, or invalid-domain-input frames.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge(bytes.len()));
        }
        let mut reader = Reader::new(bytes);
        let protocol_version = reader.u16()?;
        if protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedProtocol(protocol_version));
        }
        let expected_revision = reader.u64()?;
        let command_id = CommandId::new(reader.array()?);
        let command_version = reader.u16()?;
        if command_version != pwmtf_eight_ball::MATCH_COMMAND_VERSION {
            return Err(ProtocolError::UnsupportedCommand(command_version));
        }
        let command = decode_command(&mut reader)?;
        if !reader.finished() {
            return Err(ProtocolError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            protocol_version,
            expected_revision,
            command_id,
            command: VersionedMatchCommand {
                version: command_version,
                command,
            },
        })
    }
}

/// Selects the highest mutually supported protocol version.
///
/// # Errors
///
/// Returns [`ProtocolError::TooManyVersions`] when either offer exceeds the
/// fixed bound, or [`ProtocolError::NoCompatibleVersion`] when no version is
/// shared.
pub fn negotiate_version(client: &[u16], server: &[u16]) -> Result<u16, ProtocolError> {
    if client.len() > MAX_NEGOTIATED_VERSIONS || server.len() > MAX_NEGOTIATED_VERSIONS {
        return Err(ProtocolError::TooManyVersions);
    }
    client
        .iter()
        .filter(|version| server.contains(version))
        .copied()
        .max()
        .ok_or(ProtocolError::NoCompatibleVersion)
}

/// Snapshot sent to an authorized subscriber for initial load or reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotEnvelope {
    /// Explicit protocol version.
    pub protocol_version: u16,
    /// Canonical match revision.
    pub revision: u64,
    /// Canonical state checksum.
    pub checksum: u64,
    /// Complete versioned canonical match snapshot.
    pub snapshot: Vec<u8>,
}

impl SnapshotEnvelope {
    /// Creates a bounded current-version snapshot envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FrameTooLarge`] when the snapshot exceeds the
    /// fixed snapshot bound.
    pub fn new(revision: u64, checksum: u64, snapshot: Vec<u8>) -> Result<Self, ProtocolError> {
        if snapshot.len() > MAX_SNAPSHOT_BYTES {
            Err(ProtocolError::FrameTooLarge(snapshot.len()))
        } else {
            Ok(Self {
                protocol_version: PROTOCOL_VERSION,
                revision,
                checksum,
                snapshot,
            })
        }
    }

    /// Encodes the bounded snapshot envelope into the stable version-one wire format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(22 + self.snapshot.len());
        bytes.extend_from_slice(&self.protocol_version.to_be_bytes());
        bytes.extend_from_slice(&self.revision.to_be_bytes());
        bytes.extend_from_slice(&self.checksum.to_be_bytes());
        bytes.extend_from_slice(
            &u32::try_from(self.snapshot.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&self.snapshot);
        bytes
    }

    /// Decodes and validates a bounded version-one snapshot envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] for oversized, truncated, trailing, or unknown
    /// protocol-version payloads.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_SNAPSHOT_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge(bytes.len()));
        }
        let mut reader = Reader::new(bytes);
        let protocol_version = reader.u16()?;
        if protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedProtocol(protocol_version));
        }
        let revision = reader.u64()?;
        let checksum = reader.u64()?;
        let size = usize::try_from(reader.u32()?)
            .map_err(|_| ProtocolError::FrameTooLarge(bytes.len()))?;
        if size > MAX_SNAPSHOT_BYTES {
            return Err(ProtocolError::FrameTooLarge(size));
        }
        let snapshot = reader.bytes(size)?.to_vec();
        if !reader.finished() {
            return Err(ProtocolError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            protocol_version,
            revision,
            checksum,
            snapshot,
        })
    }
}

/// Maximum complete canonical snapshot payload.
pub const MAX_SNAPSHOT_BYTES: usize = 4_096;
/// Maximum encoded snapshot-envelope frame.
pub const MAX_SNAPSHOT_FRAME_BYTES: usize = MAX_SNAPSHOT_BYTES + 22;

/// Protocol decoding failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProtocolError {
    /// Invalid ephemeral preview wire representation.
    #[error("invalid shot preview")]
    InvalidPreview,
    /// Frame exceeds the fixed wire bound.
    #[error("frame or payload size {0} exceeds its fixed bound")]
    FrameTooLarge(usize),
    /// A version offer exceeds the negotiation bound.
    #[error("protocol version offer exceeds the fixed bound")]
    TooManyVersions,
    /// A version offer contains no versions.
    #[error("protocol version offer is empty")]
    EmptyVersionOffer,
    /// Client and server share no supported version.
    #[error("no compatible protocol version")]
    NoCompatibleVersion,
    /// Frame ended before a complete value was available.
    #[error("frame is truncated")]
    Truncated,
    /// Bytes remained after the command payload.
    #[error("frame has {0} trailing bytes")]
    TrailingBytes(usize),
    /// Transport protocol version is unknown.
    #[error("unsupported protocol version {0}")]
    UnsupportedProtocol(u16),
    /// Canonical command version is unknown.
    #[error("unsupported canonical command version {0}")]
    UnsupportedCommand(u16),
    /// Wire discriminant is unknown.
    #[error("invalid wire discriminant {0}")]
    InvalidDiscriminant(u8),
    /// Quantized aim is invalid.
    #[error("invalid quantized aim {0}")]
    InvalidAim(u16),
    /// Quantized power is invalid.
    #[error("invalid quantized power {0}")]
    InvalidPower(u16),
    /// Quantized spin is invalid.
    #[error("invalid quantized spin ({side}, {vertical})")]
    InvalidSpin {
        /// Horizontal contact offset.
        side: i16,
        /// Vertical contact offset.
        vertical: i16,
    },
}

fn encode_command(command: MatchCommand, bytes: &mut Vec<u8>) {
    match command {
        MatchCommand::PlayShot {
            shot,
            called_pocket,
        } => {
            bytes.push(1);
            bytes.extend_from_slice(&shot.version.to_be_bytes());
            bytes.extend_from_slice(&shot.aim.steps().to_be_bytes());
            bytes.extend_from_slice(&shot.power.units().to_be_bytes());
            bytes.extend_from_slice(&shot.spin.side.to_be_bytes());
            bytes.extend_from_slice(&shot.spin.vertical.to_be_bytes());
            bytes.push(encode_pocket(called_pocket));
        }
        MatchCommand::Timeout => bytes.push(2),
        MatchCommand::Concede { player } => {
            bytes.push(3);
            bytes.push(match player {
                pwmtf_eight_ball::Player::One => 1,
                pwmtf_eight_ball::Player::Two => 2,
            });
        }
        MatchCommand::PlaceCueBall { position } => {
            bytes.push(4);
            bytes.extend_from_slice(&position.x.micros().to_be_bytes());
            bytes.extend_from_slice(&position.y.micros().to_be_bytes());
        }
    }
}

fn decode_command(reader: &mut Reader<'_>) -> Result<MatchCommand, ProtocolError> {
    match reader.u8()? {
        1 => {
            let shot_version = reader.u16()?;
            if shot_version != VersionedShotCommand::VERSION {
                return Err(ProtocolError::UnsupportedCommand(shot_version));
            }
            let aim_value = reader.u16()?;
            let aim = Aim::new(aim_value).map_err(|_| ProtocolError::InvalidAim(aim_value))?;
            let power_value = reader.u16()?;
            let power = ShotPower::new(power_value)
                .map_err(|_| ProtocolError::InvalidPower(power_value))?;
            let side = reader.i16()?;
            let vertical = reader.i16()?;
            let spin = Spin::new(side, vertical)
                .map_err(|_| ProtocolError::InvalidSpin { side, vertical })?;
            let called_pocket = decode_pocket(reader.u8()?)?;
            Ok(MatchCommand::PlayShot {
                shot: VersionedShotCommand {
                    version: shot_version,
                    aim,
                    power,
                    spin,
                },
                called_pocket,
            })
        }
        2 => Ok(MatchCommand::Timeout),
        3 => {
            let player = match reader.u8()? {
                1 => pwmtf_eight_ball::Player::One,
                2 => pwmtf_eight_ball::Player::Two,
                value => return Err(ProtocolError::InvalidDiscriminant(value)),
            };
            Ok(MatchCommand::Concede { player })
        }
        4 => Ok(MatchCommand::PlaceCueBall {
            position: Vector::from_micros(reader.i64()?, reader.i64()?),
        }),
        value => Err(ProtocolError::InvalidDiscriminant(value)),
    }
}

const fn encode_pocket(pocket: Option<PocketId>) -> u8 {
    match pocket {
        None => 0,
        Some(PocketId::TopLeft) => 1,
        Some(PocketId::TopCenter) => 2,
        Some(PocketId::TopRight) => 3,
        Some(PocketId::BottomLeft) => 4,
        Some(PocketId::BottomCenter) => 5,
        Some(PocketId::BottomRight) => 6,
    }
}

const fn decode_pocket(value: u8) -> Result<Option<PocketId>, ProtocolError> {
    match value {
        0 => Ok(None),
        1 => Ok(Some(PocketId::TopLeft)),
        2 => Ok(Some(PocketId::TopCenter)),
        3 => Ok(Some(PocketId::TopRight)),
        4 => Ok(Some(PocketId::BottomLeft)),
        5 => Ok(Some(PocketId::BottomCenter)),
        6 => Ok(Some(PocketId::BottomRight)),
        _ => Err(ProtocolError::InvalidDiscriminant(value)),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn array<const SIZE: usize>(&mut self) -> Result<[u8; SIZE], ProtocolError> {
        let end = self
            .offset
            .checked_add(SIZE)
            .ok_or(ProtocolError::Truncated)?;
        let source = self
            .bytes
            .get(self.offset..end)
            .ok_or(ProtocolError::Truncated)?;
        let mut value = [0; SIZE];
        value.copy_from_slice(source);
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    fn u128(&mut self) -> Result<u128, ProtocolError> {
        Ok(u128::from_be_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    fn bytes(&mut self, size: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self
            .offset
            .checked_add(size)
            .ok_or(ProtocolError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(ProtocolError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }
    fn i16(&mut self) -> Result<i16, ProtocolError> {
        Ok(i16::from_be_bytes(self.array()?))
    }
    fn i64(&mut self) -> Result<i64, ProtocolError> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    const fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
    const fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }
}

#[cfg(test)]
mod tests {
    use pwmtf_eight_ball::{Player, VersionedMatchCommand};

    use super::*;

    #[test]
    fn preview_wire_is_bounded_and_distinct_from_commands_and_snapshots() {
        let value = ShotPreview {
            revision: 42,
            gesture: 9,
            sequence: 3,
            aim: Aim::new(120).unwrap(),
            power: ShotPower::new(777).unwrap(),
            spin: Spin::new(1_200, -2_300).unwrap(),
            cancel: false,
        };
        let bytes = value.to_bytes();
        assert_eq!(ShotPreview::from_bytes(&bytes).unwrap(), value);
        assert!(CommandEnvelope::from_bytes(&bytes).is_err());
        assert!(SnapshotEnvelope::from_bytes(&bytes).is_err());
        for malformed in [&bytes[..31], &bytes[..0]] {
            assert!(ShotPreview::from_bytes(malformed).is_err());
        }
        let mut corrupt = bytes;
        corrupt[2] = 3;
        assert!(matches!(
            ShotPreview::from_bytes(&corrupt),
            Err(ProtocolError::UnsupportedProtocol(_))
        ));
        corrupt = bytes;
        corrupt[28..30].copy_from_slice(&i16::MAX.to_be_bytes());
        assert!(ShotPreview::from_bytes(&corrupt).is_err());
        corrupt = bytes;
        corrupt[30..32].copy_from_slice(&i16::MAX.to_be_bytes());
        assert!(ShotPreview::from_bytes(&corrupt).is_err());
        corrupt = bytes;
        corrupt[27] = 2;
        assert!(ShotPreview::from_bytes(&corrupt).is_err());
        corrupt = bytes;
        corrupt[23..25].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(ShotPreview::from_bytes(&corrupt).is_err());
    }

    fn command(command: MatchCommand) -> CommandEnvelope {
        CommandEnvelope::new(
            42,
            CommandId::new([7; 16]),
            VersionedMatchCommand::new(command),
        )
    }

    #[test]
    fn snapshot_envelopes_round_trip_and_fail_closed() {
        let envelope = SnapshotEnvelope::new(7, 9, vec![1, 2, 3]).unwrap();
        let bytes = envelope.to_bytes();
        assert_eq!(SnapshotEnvelope::from_bytes(&bytes), Ok(envelope));
        assert_eq!(
            SnapshotEnvelope::from_bytes(&bytes[..bytes.len() - 1]),
            Err(ProtocolError::Truncated)
        );
        let mut unknown = bytes;
        unknown[1] = 2;
        assert_eq!(
            SnapshotEnvelope::from_bytes(&unknown),
            Err(ProtocolError::UnsupportedProtocol(2))
        );
    }

    #[test]
    fn snapshot_bounds_are_enforced() {
        assert_eq!(
            SnapshotEnvelope::new(0, 0, vec![0; MAX_SNAPSHOT_BYTES + 1]),
            Err(ProtocolError::FrameTooLarge(MAX_SNAPSHOT_BYTES + 1))
        );
    }

    #[test]
    fn version_negotiation_selects_highest_shared_version() {
        assert_eq!(negotiate_version(&[1, 3, 2], &[1, 2]), Ok(2));
        assert_eq!(
            negotiate_version(&[2], &[1]),
            Err(ProtocolError::NoCompatibleVersion)
        );
        assert_eq!(
            negotiate_version(&[1; MAX_NEGOTIATED_VERSIONS + 1], &[1]),
            Err(ProtocolError::TooManyVersions)
        );
    }

    #[test]
    fn every_command_round_trips() {
        let commands = [
            MatchCommand::Timeout,
            MatchCommand::Concede {
                player: Player::Two,
            },
            MatchCommand::PlaceCueBall {
                position: Vector::from_micros(-123, 456),
            },
            MatchCommand::PlayShot {
                shot: VersionedShotCommand::new(
                    Aim::new(123).unwrap(),
                    ShotPower::new(4_567).unwrap(),
                    Spin::new(-200, 300).unwrap(),
                ),
                called_pocket: Some(PocketId::BottomRight),
            },
        ];
        for payload in commands {
            let envelope = command(payload);
            assert_eq!(
                CommandEnvelope::from_bytes(&envelope.to_bytes()),
                Ok(envelope)
            );
        }
    }

    #[test]
    fn malformed_frames_fail_closed() {
        let envelope = command(MatchCommand::Timeout);
        let bytes = envelope.to_bytes();
        assert_eq!(
            CommandEnvelope::from_bytes(&bytes[..bytes.len() - 1]),
            Err(ProtocolError::Truncated)
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            CommandEnvelope::from_bytes(&trailing),
            Err(ProtocolError::TrailingBytes(1))
        );
        let mut unknown = bytes;
        unknown[1] = 2;
        assert_eq!(
            CommandEnvelope::from_bytes(&unknown),
            Err(ProtocolError::UnsupportedProtocol(2))
        );
        assert_eq!(
            CommandEnvelope::from_bytes(&vec![0; MAX_FRAME_BYTES + 1]),
            Err(ProtocolError::FrameTooLarge(MAX_FRAME_BYTES + 1))
        );
    }
}
