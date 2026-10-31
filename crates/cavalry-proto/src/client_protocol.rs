//! The client protocol's commands and events: `spec/client-protocol.md`, version 2
//! (ADR 0019; the recording commands and events are ADR 0025, the replay commands and
//! events ADR 0030).
//!
//! A [`Command`] is the message of a command ring record whose type is a
//! [`CommandType`]; an [`Event`] is the message of a broadcast buffer record whose type
//! is an [`EventType`]. [`Command::parse`] and [`Event::parse`] run the spec's decoding
//! checks in the spec's order and borrow the strings from the message, so decoding
//! allocates nothing; [`Command::write`] and [`Event::write`] put a message in place.
//! What the driver does with a decoded command, and the checks it runs against its own
//! state, are the driver's; this module is the bytes.
//!
//! ```
//! use cavalry_proto::client_protocol::{Command, CommandType, ProtocolError};
//!
//! fn main() -> Result<(), ProtocolError> {
//!     // A client: the command, written into a command ring claim of the right length.
//!     let command = Command::AddPublication {
//!         client_id: 0,
//!         correlation_id: 1,
//!         stream_id: 1001,
//!         channel: b"cavalry:ipc",
//!     };
//!     let mut claim = [0u8; 64];
//!     let length = command.write(&mut claim)?;
//!     assert_eq!(length, command.encoded_length()?);
//!     assert_eq!(length, 36); // 16 bytes of ids, a stream id, and a padded string
//!
//!     // The driver: the record's type and message, decoded in place.
//!     let decoded = Command::parse(CommandType::AddPublication as i32, &claim[..length])?;
//!     assert_eq!(decoded, command);
//!     assert_eq!(decoded.correlation_id(), 1);
//!
//!     // A message longer than its fields is refused, never trusted.
//!     let rejected = Command::parse(CommandType::AddPublication as i32, &claim[..40]).unwrap_err();
//!     assert_eq!(rejected.reason(), "length");
//!     println!("{rejected}");
//!     Ok(())
//! }
//! ```

use core::fmt;

/// The version of `spec/client-protocol.md` this crate reads and writes.
pub const VERSION: i32 = 2;

/// The longest string field, in bytes: `spec/client-protocol.md`, "Encoding".
pub const MAX_STRING_LENGTH: usize = 4096;

/// The bytes every command begins with: the client id and the correlation id.
pub const CORRELATED_LENGTH: usize = 16;

/// The channel every publication and subscription of this version names.
pub const IPC_CHANNEL: &[u8] = b"cavalry:ipc";

/// The position a `start replay` passes for the recording's start position: Aeron's
/// `NULL_POSITION` (`spec/client-protocol.md`, "Commands"; ADR 0030).
pub const NULL_POSITION: i64 = -1;

/// The length a `start replay` passes for everything up to the recording's extent when
/// the replay starts: Aeron's `NULL_LENGTH`.
pub const NULL_LENGTH: i64 = -1;

/// The length a `start replay` passes to follow a live recording until it stops:
/// Aeron's `Long.MAX_VALUE`.
pub const FOLLOW_LENGTH: i64 = i64::MAX;

/// How many replays the driver holds, starting or live: its replays registry's capacity
/// (`spec/client-protocol.md`, "Sizes"; ADR 0030). The client's conductor follows at
/// most this many replays for one process, since the driver admits no more, so a
/// `start replay` past it is refused by the driver with error code 10, or by the client
/// with `ReplaysFull`, both naming this number; it lives here so that the two sides and
/// the how-to's "64" move together (ADR 0036).
pub const MAX_REPLAYS: usize = 64;

/// The docs anchor every error message names.
const DOC: &str = "docs/reference/client-protocol.md#what-the-driver-rejects";

/// The type id of a command ring record that carries a command: `spec/client-protocol.md`,
/// "Commands".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum CommandType {
    /// Add a concurrent publication, or join a live one on the same stream and channel.
    AddPublication = 1,
    /// Remove a client's publication registration.
    RemovePublication = 2,
    /// Add an exclusive publication: a new session every time.
    AddExclusivePublication = 3,
    /// Add a subscription.
    AddSubscription = 4,
    /// Remove a subscription and its images.
    RemoveSubscription = 5,
    /// Refresh the client's liveness. No answer.
    ClientKeepalive = 6,
    /// Remove everything of the client's and forget it. No answer.
    ClientClose = 11,
    /// Record the live publication on a stream and channel to the durable log; the first
    /// of Cavalry's own commands, in its range from `0x0101` (ADR 0025).
    StartRecording = 0x0101,
    /// Stop a recording at the position the recorder has written.
    StopRecording = 0x0102,
    /// Play a recording from a position into a publication on a replay stream, which
    /// a subscription reads as any stream (ADR 0030).
    StartReplay = 0x0103,
    /// Stop a replay at the position reached.
    StopReplay = 0x0104,
}

impl TryFrom<i32> for CommandType {
    type Error = ProtocolError;

    /// The command for a record's type id, or [`ProtocolError::Type`] for a reserved or
    /// unknown one.
    fn try_from(value: i32) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(CommandType::AddPublication),
            2 => Ok(CommandType::RemovePublication),
            3 => Ok(CommandType::AddExclusivePublication),
            4 => Ok(CommandType::AddSubscription),
            5 => Ok(CommandType::RemoveSubscription),
            6 => Ok(CommandType::ClientKeepalive),
            11 => Ok(CommandType::ClientClose),
            0x0101 => Ok(CommandType::StartRecording),
            0x0102 => Ok(CommandType::StopRecording),
            0x0103 => Ok(CommandType::StartReplay),
            0x0104 => Ok(CommandType::StopReplay),
            found => Err(ProtocolError::Type { found }),
        }
    }
}

/// The type id of a broadcast buffer record that carries an event:
/// `spec/client-protocol.md`, "Events".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum EventType {
    /// A command was refused; the code and message say why.
    Error = 0x0F01,
    /// A subscription has a new image of a publication.
    AvailableImage = 0x0F02,
    /// A concurrent publication is registered and its log exists.
    PublicationReady = 0x0F03,
    /// A remove command succeeded.
    OperationSuccess = 0x0F04,
    /// An image's publication is gone and its log is about to be removed.
    UnavailableImage = 0x0F05,
    /// An exclusive publication is registered and its log exists.
    ExclusivePublicationReady = 0x0F06,
    /// A subscription is registered.
    SubscriptionReady = 0x0F07,
    /// The driver timed a client out and removed everything of its.
    ClientTimeout = 0x0F0A,
    /// A recording is in the catalog with its files and its two counters; the first of
    /// Cavalry's own events, in its range from `0x1001` (ADR 0025).
    RecordingReady = 0x1001,
    /// A recording stopped, by command, at its publication's end of stream, or on a
    /// failure; sent to every client.
    RecordingStopped = 0x1002,
    /// A replay's publication exists at the recording's start position, with its
    /// `replay.position` counter (ADR 0030).
    ReplayReady = 0x1003,
    /// A replay ended, by reaching its end, by command, for want of a subscriber, or on
    /// a failure; sent to every client.
    ReplayStopped = 0x1004,
}

impl TryFrom<i32> for EventType {
    type Error = ProtocolError;

    /// The event for a record's type id, or [`ProtocolError::Type`] for a reserved or
    /// unknown one.
    fn try_from(value: i32) -> Result<Self, ProtocolError> {
        match value {
            0x0F01 => Ok(EventType::Error),
            0x0F02 => Ok(EventType::AvailableImage),
            0x0F03 => Ok(EventType::PublicationReady),
            0x0F04 => Ok(EventType::OperationSuccess),
            0x0F05 => Ok(EventType::UnavailableImage),
            0x0F06 => Ok(EventType::ExclusivePublicationReady),
            0x0F07 => Ok(EventType::SubscriptionReady),
            0x0F0A => Ok(EventType::ClientTimeout),
            0x1001 => Ok(EventType::RecordingReady),
            0x1002 => Ok(EventType::RecordingStopped),
            0x1003 => Ok(EventType::ReplayReady),
            0x1004 => Ok(EventType::ReplayStopped),
            found => Err(ProtocolError::Type { found }),
        }
    }
}

/// The code of an `error` event: `spec/client-protocol.md`, "Error codes". Aeron's
/// numbers up to 14, the ones this version does not use reserved with no variant, and
/// Cavalry's own from 20 (ADR 0025).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ErrorCode {
    /// The channel is not `cavalry:ipc`.
    InvalidChannel = 1,
    /// Remove subscription named a registration the driver does not know, or another
    /// client's.
    UnknownSubscription = 2,
    /// Remove publication named a registration the driver does not know, or another
    /// client's.
    UnknownPublication = 3,
    /// The record's type is not a command of this version.
    UnknownCommandType = 6,
    /// The message failed a decoding check after its first 16 bytes were read.
    MalformedCommand = 7,
    /// A registry is at capacity or the counters file has no free record.
    ResourceTemporarilyUnavailable = 10,
    /// Anything else; the message says what.
    GenericError = 11,
    /// The log file, or a recording's time index or segment file, could not be created
    /// or sized.
    StorageSpace = 12,
    /// Start recording named a stream id and channel no live publication has.
    NoPublicationToRecord = 20,
    /// Stop recording named a recording id the catalog does not hold.
    UnknownRecording = 21,
    /// Start recording named a stream whose live publications are all being recorded.
    AlreadyRecording = 22,
    /// Start replay named a position that is not a multiple of 32, below the
    /// recording's start position or above its extent.
    PositionOutOfRange = 23,
}

impl ErrorCode {
    /// The code for a number, or `None` for one this version does not send.
    pub const fn from_code(code: i32) -> Option<ErrorCode> {
        match code {
            1 => Some(ErrorCode::InvalidChannel),
            2 => Some(ErrorCode::UnknownSubscription),
            3 => Some(ErrorCode::UnknownPublication),
            6 => Some(ErrorCode::UnknownCommandType),
            7 => Some(ErrorCode::MalformedCommand),
            10 => Some(ErrorCode::ResourceTemporarilyUnavailable),
            11 => Some(ErrorCode::GenericError),
            12 => Some(ErrorCode::StorageSpace),
            20 => Some(ErrorCode::NoPublicationToRecord),
            21 => Some(ErrorCode::UnknownRecording),
            22 => Some(ErrorCode::AlreadyRecording),
            23 => Some(ErrorCode::PositionOutOfRange),
            _ => None,
        }
    }

    /// The number on the wire.
    pub const fn code(self) -> i32 {
        self as i32
    }
}

/// Why a replay stopped: the `reason` of a `replay stopped` event
/// (`spec/client-protocol.md`, "Events"; ADR 0030).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ReplayStopReason {
    /// The replay reached its end position or the recording's stop position.
    Ended = 0,
    /// A `stop replay` command ended it at the position reached.
    Stopped = 1,
    /// The replay's publication had no subscriber for the replay connect timeout.
    NotConnected = 2,
    /// A segment file could not be read, or a frame failed a check; the position is
    /// the last frame delivered.
    Failed = 3,
}

impl ReplayStopReason {
    /// The reason for a number, or `None` for one this version does not send.
    pub const fn from_code(code: i32) -> Option<ReplayStopReason> {
        match code {
            0 => Some(ReplayStopReason::Ended),
            1 => Some(ReplayStopReason::Stopped),
            2 => Some(ReplayStopReason::NotConnected),
            3 => Some(ReplayStopReason::Failed),
            _ => None,
        }
    }

    /// The number on the wire.
    pub const fn code(self) -> i32 {
        self as i32
    }
}

/// Why a message was refused. The variants are the spec's decoding checks, and
/// [`ProtocolError::reason`] is the name the spec and the vectors use. A writer
/// reports the same variants for a buffer too small or a string too long.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProtocolError {
    /// The message, or the buffer to write into, is shorter than the fields need.
    TooShort {
        /// How many bytes there are.
        given: usize,
        /// How many the fields need.
        needed: usize,
    },
    /// The type id is not a command or event of this version.
    Type {
        /// The type id found.
        found: i32,
    },
    /// A string's length is below 0, above [`MAX_STRING_LENGTH`], or its bytes or
    /// padding run past the message.
    String {
        /// The length found, or asked for.
        length: i64,
        /// The bytes left for the string and its padding, after its length field.
        available: usize,
    },
    /// The message is longer than its fields.
    Length {
        /// The message's length.
        given: usize,
        /// The fields' length.
        expected: usize,
    },
}

impl ProtocolError {
    /// The reason's name in `spec/client-protocol.md`, "What the driver rejects":
    /// `too-short`, `type`, `string` or `length`.
    pub const fn reason(self) -> &'static str {
        match self {
            ProtocolError::TooShort { .. } => "too-short",
            ProtocolError::Type { .. } => "type",
            ProtocolError::String { .. } => "string",
            ProtocolError::Length { .. } => "length",
        }
    }

    /// The code the driver answers a refused command with, or `None` when the message
    /// was too short to carry a correlation id, so there is nothing to answer
    /// (`spec/client-protocol.md`, "What the driver rejects").
    pub const fn error_code(self) -> Option<ErrorCode> {
        match self {
            ProtocolError::TooShort { given, .. } if given < CORRELATED_LENGTH => None,
            ProtocolError::Type { .. } => Some(ErrorCode::UnknownCommandType),
            _ => Some(ErrorCode::MalformedCommand),
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ProtocolError::TooShort { given, needed } => write!(
                f,
                "message rejected (too-short): {given} bytes, the fields need {needed}; \
                 the record is truncated or the writer runs another Cavalry version; see {DOC}"
            ),
            ProtocolError::Type { found } => write!(
                f,
                "message rejected (type): type {found} is not a command or event of \
                 version {VERSION}; the writer runs another Cavalry version, upgrade the \
                 older side; see {DOC}"
            ),
            ProtocolError::String { length, available } => write!(
                f,
                "message rejected (string): a string of length {length} with {available} \
                 bytes available, the bound is 0 to {MAX_STRING_LENGTH} inside the message; \
                 the record is corrupt or the writer does not conform; see {DOC}"
            ),
            ProtocolError::Length { given, expected } => write!(
                f,
                "message rejected (length): {given} bytes, the fields take {expected}; \
                 the writer runs another Cavalry version or does not conform; see {DOC}"
            ),
        }
    }
}

impl core::error::Error for ProtocolError {}

/// A command, decoded from or to be written as a command ring record's message:
/// `spec/client-protocol.md`, "Commands". Strings borrow the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command<'a> {
    /// Type 1.
    AddPublication {
        /// The client's id.
        client_id: i64,
        /// This command's id; the answer names it.
        correlation_id: i64,
        /// The stream to publish on.
        stream_id: i32,
        /// The channel, `cavalry:ipc` in this version.
        channel: &'a [u8],
    },
    /// Type 2.
    RemovePublication {
        /// The client's id.
        client_id: i64,
        /// This command's id; the answer names it.
        correlation_id: i64,
        /// The client's registration: the correlation id of its add command.
        registration_id: i64,
    },
    /// Type 3.
    AddExclusivePublication {
        /// The client's id.
        client_id: i64,
        /// This command's id; the answer names it.
        correlation_id: i64,
        /// The stream to publish on.
        stream_id: i32,
        /// The channel, `cavalry:ipc` in this version.
        channel: &'a [u8],
    },
    /// Type 4.
    AddSubscription {
        /// The client's id.
        client_id: i64,
        /// This command's id, which becomes the subscription's registration id.
        correlation_id: i64,
        /// The stream to subscribe to.
        stream_id: i32,
        /// The channel, `cavalry:ipc` in this version.
        channel: &'a [u8],
    },
    /// Type 5.
    RemoveSubscription {
        /// The client's id.
        client_id: i64,
        /// This command's id; the answer names it.
        correlation_id: i64,
        /// The subscription's registration id.
        registration_id: i64,
    },
    /// Type 6. No answer.
    ClientKeepalive {
        /// The client's id.
        client_id: i64,
        /// This command's id.
        correlation_id: i64,
    },
    /// Type 11. No answer.
    ClientClose {
        /// The client's id.
        client_id: i64,
        /// This command's id.
        correlation_id: i64,
    },
    /// Type 257 (`0x0101`).
    StartRecording {
        /// The client's id.
        client_id: i64,
        /// This command's id; `recording ready` or `error` names it.
        correlation_id: i64,
        /// The stream whose live publication is recorded.
        stream_id: i32,
        /// The channel, `cavalry:ipc` in this version.
        channel: &'a [u8],
    },
    /// Type 258 (`0x0102`).
    StopRecording {
        /// The client's id.
        client_id: i64,
        /// This command's id; `operation success` or `error` names it.
        correlation_id: i64,
        /// The recording id that `recording ready` carried.
        recording_id: i64,
    },
    /// Type 259 (`0x0103`).
    StartReplay {
        /// The client's id.
        client_id: i64,
        /// This command's id, which becomes the replay session id; `replay ready` or
        /// `error` names it.
        correlation_id: i64,
        /// The recording to play.
        recording_id: i64,
        /// Where to start: a multiple of 32 from the recording's start position to its
        /// extent, or [`NULL_POSITION`] for the start position.
        position: i64,
        /// How many bytes to play: 0 or more, [`NULL_LENGTH`] for everything up to the
        /// extent when the replay starts, or [`FOLLOW_LENGTH`] to follow a live
        /// recording until it stops.
        length: i64,
        /// The stream the replay's publication is on.
        replay_stream_id: i32,
        /// The replay publication's channel, `cavalry:ipc` in this version.
        replay_channel: &'a [u8],
    },
    /// Type 260 (`0x0104`).
    StopReplay {
        /// The client's id.
        client_id: i64,
        /// This command's id; `operation success` or `error` names it.
        correlation_id: i64,
        /// The replay session id that `replay ready` carried.
        replay_session_id: i64,
    },
}

impl<'a> Command<'a> {
    /// The record type this command is written under.
    pub const fn command_type(&self) -> CommandType {
        match self {
            Command::AddPublication { .. } => CommandType::AddPublication,
            Command::RemovePublication { .. } => CommandType::RemovePublication,
            Command::AddExclusivePublication { .. } => CommandType::AddExclusivePublication,
            Command::AddSubscription { .. } => CommandType::AddSubscription,
            Command::RemoveSubscription { .. } => CommandType::RemoveSubscription,
            Command::ClientKeepalive { .. } => CommandType::ClientKeepalive,
            Command::ClientClose { .. } => CommandType::ClientClose,
            Command::StartRecording { .. } => CommandType::StartRecording,
            Command::StopRecording { .. } => CommandType::StopRecording,
            Command::StartReplay { .. } => CommandType::StartReplay,
            Command::StopReplay { .. } => CommandType::StopReplay,
        }
    }

    /// The client id every command begins with.
    pub const fn client_id(&self) -> i64 {
        match *self {
            Command::AddPublication { client_id, .. }
            | Command::RemovePublication { client_id, .. }
            | Command::AddExclusivePublication { client_id, .. }
            | Command::AddSubscription { client_id, .. }
            | Command::RemoveSubscription { client_id, .. }
            | Command::ClientKeepalive { client_id, .. }
            | Command::ClientClose { client_id, .. }
            | Command::StartRecording { client_id, .. }
            | Command::StopRecording { client_id, .. }
            | Command::StartReplay { client_id, .. }
            | Command::StopReplay { client_id, .. } => client_id,
        }
    }

    /// The correlation id every command carries after the client id.
    pub const fn correlation_id(&self) -> i64 {
        match *self {
            Command::AddPublication { correlation_id, .. }
            | Command::RemovePublication { correlation_id, .. }
            | Command::AddExclusivePublication { correlation_id, .. }
            | Command::AddSubscription { correlation_id, .. }
            | Command::RemoveSubscription { correlation_id, .. }
            | Command::ClientKeepalive { correlation_id, .. }
            | Command::ClientClose { correlation_id, .. }
            | Command::StartRecording { correlation_id, .. }
            | Command::StopRecording { correlation_id, .. }
            | Command::StartReplay { correlation_id, .. }
            | Command::StopReplay { correlation_id, .. } => correlation_id,
        }
    }

    /// Decodes the message of a command ring record of type `type_id`, running the
    /// spec's checks in the spec's order: a message shorter than
    /// [`CORRELATED_LENGTH`] is `too-short` before the type is looked at, since there
    /// is no correlation id to answer; then the type; then the fields, each string's
    /// bound, and the message's length against the fields'.
    pub fn parse(type_id: i32, message: &'a [u8]) -> Result<Self, ProtocolError> {
        if message.len() < CORRELATED_LENGTH {
            return Err(ProtocolError::TooShort {
                given: message.len(),
                needed: CORRELATED_LENGTH,
            });
        }
        let command_type = CommandType::try_from(type_id)?;
        let mut cursor = Cursor::new(message);
        let client_id = cursor.i64()?;
        let correlation_id = cursor.i64()?;
        let command = match command_type {
            CommandType::AddPublication => Command::AddPublication {
                client_id,
                correlation_id,
                stream_id: cursor.i32()?,
                channel: cursor.string()?,
            },
            CommandType::RemovePublication => Command::RemovePublication {
                client_id,
                correlation_id,
                registration_id: cursor.i64()?,
            },
            CommandType::AddExclusivePublication => Command::AddExclusivePublication {
                client_id,
                correlation_id,
                stream_id: cursor.i32()?,
                channel: cursor.string()?,
            },
            CommandType::AddSubscription => Command::AddSubscription {
                client_id,
                correlation_id,
                stream_id: cursor.i32()?,
                channel: cursor.string()?,
            },
            CommandType::RemoveSubscription => Command::RemoveSubscription {
                client_id,
                correlation_id,
                registration_id: cursor.i64()?,
            },
            CommandType::ClientKeepalive => Command::ClientKeepalive {
                client_id,
                correlation_id,
            },
            CommandType::ClientClose => Command::ClientClose {
                client_id,
                correlation_id,
            },
            CommandType::StartRecording => Command::StartRecording {
                client_id,
                correlation_id,
                stream_id: cursor.i32()?,
                channel: cursor.string()?,
            },
            CommandType::StopRecording => Command::StopRecording {
                client_id,
                correlation_id,
                recording_id: cursor.i64()?,
            },
            CommandType::StartReplay => Command::StartReplay {
                client_id,
                correlation_id,
                recording_id: cursor.i64()?,
                position: cursor.i64()?,
                length: cursor.i64()?,
                replay_stream_id: cursor.i32()?,
                replay_channel: cursor.string()?,
            },
            CommandType::StopReplay => Command::StopReplay {
                client_id,
                correlation_id,
                replay_session_id: cursor.i64()?,
            },
        };
        cursor.finish()?;
        Ok(command)
    }

    /// The message's length in bytes, or [`ProtocolError::String`] when a string is
    /// longer than [`MAX_STRING_LENGTH`].
    pub fn encoded_length(&self) -> Result<usize, ProtocolError> {
        let mut length = Measure(0);
        self.encode(&mut length)?;
        Ok(length.0)
    }

    /// Writes the message into the front of `buffer` and returns its length. Fails with
    /// [`ProtocolError::String`] when a string is longer than [`MAX_STRING_LENGTH`] and
    /// with [`ProtocolError::TooShort`] when `buffer` is shorter than the message;
    /// nothing is written then.
    pub fn write(&self, buffer: &mut [u8]) -> Result<usize, ProtocolError> {
        let needed = self.encoded_length()?;
        if buffer.len() < needed {
            return Err(ProtocolError::TooShort {
                given: buffer.len(),
                needed,
            });
        }
        let mut writer = Writer { buffer, pos: 0 };
        self.encode(&mut writer)?;
        Ok(writer.pos)
    }

    fn encode(&self, out: &mut impl Sink) -> Result<(), ProtocolError> {
        out.i64(self.client_id())?;
        out.i64(self.correlation_id())?;
        match *self {
            Command::AddPublication {
                stream_id, channel, ..
            }
            | Command::AddExclusivePublication {
                stream_id, channel, ..
            }
            | Command::AddSubscription {
                stream_id, channel, ..
            }
            | Command::StartRecording {
                stream_id, channel, ..
            } => {
                out.i32(stream_id)?;
                out.string(channel)
            }
            Command::RemovePublication {
                registration_id, ..
            }
            | Command::RemoveSubscription {
                registration_id, ..
            } => out.i64(registration_id),
            Command::StopRecording { recording_id, .. } => out.i64(recording_id),
            Command::StartReplay {
                recording_id,
                position,
                length,
                replay_stream_id,
                replay_channel,
                ..
            } => {
                out.i64(recording_id)?;
                out.i64(position)?;
                out.i64(length)?;
                out.i32(replay_stream_id)?;
                out.string(replay_channel)
            }
            Command::StopReplay {
                replay_session_id, ..
            } => out.i64(replay_session_id),
            Command::ClientKeepalive { .. } | Command::ClientClose { .. } => Ok(()),
        }
    }
}

/// An event, decoded from or to be written as a broadcast buffer record's message:
/// `spec/client-protocol.md`, "Events". Strings borrow the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event<'a> {
    /// Type `0x0F01`: a command was refused.
    Error {
        /// The correlation id of the command refused.
        offending_correlation_id: i64,
        /// One of [`ErrorCode`]'s numbers.
        error_code: i32,
        /// What the operator does next, ending with the page and section that explains it.
        error_message: &'a [u8],
    },
    /// Type `0x0F02`: a subscription has a new image.
    AvailableImage {
        /// The publication's registration id, which names the image.
        correlation_id: i64,
        /// The publication's session.
        session_id: i32,
        /// The stream.
        stream_id: i32,
        /// The subscription the image belongs to.
        subscriber_registration_id: i64,
        /// The `subscription.position` counter the client owns for this image.
        subscriber_position_counter_id: i32,
        /// The log's path relative to the driver directory.
        log_file_name: &'a [u8],
        /// The publisher's channel.
        source_identity: &'a [u8],
    },
    /// Type `0x0F03`: a concurrent publication is ready.
    PublicationReady {
        /// The add command's correlation id.
        correlation_id: i64,
        /// The publication's registration id: the correlation id unless the command
        /// joined a live publication.
        registration_id: i64,
        /// The publication's session.
        session_id: i32,
        /// The stream.
        stream_id: i32,
        /// The `publication.limit` counter the publisher loads.
        publication_limit_counter_id: i32,
        /// The log's path relative to the driver directory.
        log_file_name: &'a [u8],
    },
    /// Type `0x0F04`: a remove command succeeded.
    OperationSuccess {
        /// The command's correlation id.
        correlation_id: i64,
    },
    /// Type `0x0F05`: an image's publication is gone.
    UnavailableImage {
        /// The image's correlation id, as `AvailableImage` named it.
        correlation_id: i64,
        /// The subscription the image belonged to.
        subscriber_registration_id: i64,
        /// The stream.
        stream_id: i32,
        /// The publisher's channel.
        channel: &'a [u8],
    },
    /// Type `0x0F06`: an exclusive publication is ready; the fields of `PublicationReady`.
    ExclusivePublicationReady {
        /// The add command's correlation id.
        correlation_id: i64,
        /// The publication's registration id, equal to the correlation id.
        registration_id: i64,
        /// The publication's session.
        session_id: i32,
        /// The stream.
        stream_id: i32,
        /// The `publication.limit` counter the publisher loads.
        publication_limit_counter_id: i32,
        /// The log's path relative to the driver directory.
        log_file_name: &'a [u8],
    },
    /// Type `0x0F07`: a subscription is registered.
    SubscriptionReady {
        /// The add command's correlation id, the subscription's registration id.
        correlation_id: i64,
    },
    /// Type `0x0F0A`: a client was timed out.
    ClientTimeout {
        /// The client the driver timed out.
        client_id: i64,
    },
    /// Type `0x1001`: a recording started and its counters exist.
    RecordingReady {
        /// The start recording command's correlation id.
        correlation_id: i64,
        /// The recording's slot in the catalog.
        recording_id: i64,
        /// The session of the publication being recorded.
        session_id: i32,
        /// The `recording.position` counter: what the operating system holds.
        recording_position_counter_id: i32,
        /// The `recording.synced_position` counter: what a sync has made durable.
        recording_synced_position_counter_id: i32,
    },
    /// Type `0x1002`: a recording stopped; sent to every client.
    RecordingStopped {
        /// The recording.
        recording_id: i64,
        /// The catalog's stop position.
        stop_position: i64,
    },
    /// Type `0x1003`: a replay's publication exists and its counter is allocated.
    ReplayReady {
        /// The start replay command's correlation id.
        correlation_id: i64,
        /// The replay session id: the command's correlation id, which names the log and
        /// which `stop replay` names.
        replay_session_id: i64,
        /// The recording being played.
        recording_id: i64,
        /// Where the replay starts: the recording's start position when the command
        /// passed [`NULL_POSITION`].
        start_position: i64,
        /// The replay publication's session, which its images carry.
        session_id: i32,
        /// The replay stream id.
        stream_id: i32,
    },
    /// Type `0x1004`: a replay stopped; sent to every client.
    ReplayStopped {
        /// The replay.
        replay_session_id: i64,
        /// The position after the last frame written.
        position: i64,
        /// One of [`ReplayStopReason`]'s numbers.
        reason: i32,
    },
}

impl<'a> Event<'a> {
    /// The record type this event is written under.
    pub const fn event_type(&self) -> EventType {
        match self {
            Event::Error { .. } => EventType::Error,
            Event::AvailableImage { .. } => EventType::AvailableImage,
            Event::PublicationReady { .. } => EventType::PublicationReady,
            Event::OperationSuccess { .. } => EventType::OperationSuccess,
            Event::UnavailableImage { .. } => EventType::UnavailableImage,
            Event::ExclusivePublicationReady { .. } => EventType::ExclusivePublicationReady,
            Event::SubscriptionReady { .. } => EventType::SubscriptionReady,
            Event::ClientTimeout { .. } => EventType::ClientTimeout,
            Event::RecordingReady { .. } => EventType::RecordingReady,
            Event::RecordingStopped { .. } => EventType::RecordingStopped,
            Event::ReplayReady { .. } => EventType::ReplayReady,
            Event::ReplayStopped { .. } => EventType::ReplayStopped,
        }
    }

    /// Decodes the message of a broadcast buffer record of type `type_id`: the type
    /// first, then the fields, each string's bound, and the message's length against
    /// the fields'. An event has no 16-byte prefix, so there is no check before the type.
    pub fn parse(type_id: i32, message: &'a [u8]) -> Result<Self, ProtocolError> {
        let event_type = EventType::try_from(type_id)?;
        let mut cursor = Cursor::new(message);
        let event = match event_type {
            EventType::Error => Event::Error {
                offending_correlation_id: cursor.i64()?,
                error_code: cursor.i32()?,
                error_message: cursor.string()?,
            },
            EventType::AvailableImage => Event::AvailableImage {
                correlation_id: cursor.i64()?,
                session_id: cursor.i32()?,
                stream_id: cursor.i32()?,
                subscriber_registration_id: cursor.i64()?,
                subscriber_position_counter_id: cursor.i32()?,
                log_file_name: cursor.string()?,
                source_identity: cursor.string()?,
            },
            EventType::PublicationReady => Event::PublicationReady {
                correlation_id: cursor.i64()?,
                registration_id: cursor.i64()?,
                session_id: cursor.i32()?,
                stream_id: cursor.i32()?,
                publication_limit_counter_id: cursor.i32()?,
                log_file_name: cursor.string()?,
            },
            EventType::OperationSuccess => Event::OperationSuccess {
                correlation_id: cursor.i64()?,
            },
            EventType::UnavailableImage => Event::UnavailableImage {
                correlation_id: cursor.i64()?,
                subscriber_registration_id: cursor.i64()?,
                stream_id: cursor.i32()?,
                channel: cursor.string()?,
            },
            EventType::ExclusivePublicationReady => Event::ExclusivePublicationReady {
                correlation_id: cursor.i64()?,
                registration_id: cursor.i64()?,
                session_id: cursor.i32()?,
                stream_id: cursor.i32()?,
                publication_limit_counter_id: cursor.i32()?,
                log_file_name: cursor.string()?,
            },
            EventType::SubscriptionReady => Event::SubscriptionReady {
                correlation_id: cursor.i64()?,
            },
            EventType::ClientTimeout => Event::ClientTimeout {
                client_id: cursor.i64()?,
            },
            EventType::RecordingReady => Event::RecordingReady {
                correlation_id: cursor.i64()?,
                recording_id: cursor.i64()?,
                session_id: cursor.i32()?,
                recording_position_counter_id: cursor.i32()?,
                recording_synced_position_counter_id: cursor.i32()?,
            },
            EventType::RecordingStopped => Event::RecordingStopped {
                recording_id: cursor.i64()?,
                stop_position: cursor.i64()?,
            },
            EventType::ReplayReady => Event::ReplayReady {
                correlation_id: cursor.i64()?,
                replay_session_id: cursor.i64()?,
                recording_id: cursor.i64()?,
                start_position: cursor.i64()?,
                session_id: cursor.i32()?,
                stream_id: cursor.i32()?,
            },
            EventType::ReplayStopped => Event::ReplayStopped {
                replay_session_id: cursor.i64()?,
                position: cursor.i64()?,
                reason: cursor.i32()?,
            },
        };
        cursor.finish()?;
        Ok(event)
    }

    /// The message's length in bytes, or [`ProtocolError::String`] when a string is
    /// longer than [`MAX_STRING_LENGTH`].
    pub fn encoded_length(&self) -> Result<usize, ProtocolError> {
        let mut length = Measure(0);
        self.encode(&mut length)?;
        Ok(length.0)
    }

    /// Writes the message into the front of `buffer` and returns its length, with the
    /// failures of [`Command::write`].
    pub fn write(&self, buffer: &mut [u8]) -> Result<usize, ProtocolError> {
        let needed = self.encoded_length()?;
        if buffer.len() < needed {
            return Err(ProtocolError::TooShort {
                given: buffer.len(),
                needed,
            });
        }
        let mut writer = Writer { buffer, pos: 0 };
        self.encode(&mut writer)?;
        Ok(writer.pos)
    }

    fn encode(&self, out: &mut impl Sink) -> Result<(), ProtocolError> {
        match *self {
            Event::Error {
                offending_correlation_id,
                error_code,
                error_message,
            } => {
                out.i64(offending_correlation_id)?;
                out.i32(error_code)?;
                out.string(error_message)
            }
            Event::AvailableImage {
                correlation_id,
                session_id,
                stream_id,
                subscriber_registration_id,
                subscriber_position_counter_id,
                log_file_name,
                source_identity,
            } => {
                out.i64(correlation_id)?;
                out.i32(session_id)?;
                out.i32(stream_id)?;
                out.i64(subscriber_registration_id)?;
                out.i32(subscriber_position_counter_id)?;
                out.string(log_file_name)?;
                out.string(source_identity)
            }
            Event::PublicationReady {
                correlation_id,
                registration_id,
                session_id,
                stream_id,
                publication_limit_counter_id,
                log_file_name,
            }
            | Event::ExclusivePublicationReady {
                correlation_id,
                registration_id,
                session_id,
                stream_id,
                publication_limit_counter_id,
                log_file_name,
            } => {
                out.i64(correlation_id)?;
                out.i64(registration_id)?;
                out.i32(session_id)?;
                out.i32(stream_id)?;
                out.i32(publication_limit_counter_id)?;
                out.string(log_file_name)
            }
            Event::OperationSuccess { correlation_id }
            | Event::SubscriptionReady { correlation_id } => out.i64(correlation_id),
            Event::UnavailableImage {
                correlation_id,
                subscriber_registration_id,
                stream_id,
                channel,
            } => {
                out.i64(correlation_id)?;
                out.i64(subscriber_registration_id)?;
                out.i32(stream_id)?;
                out.string(channel)
            }
            Event::ClientTimeout { client_id } => out.i64(client_id),
            Event::RecordingReady {
                correlation_id,
                recording_id,
                session_id,
                recording_position_counter_id,
                recording_synced_position_counter_id,
            } => {
                out.i64(correlation_id)?;
                out.i64(recording_id)?;
                out.i32(session_id)?;
                out.i32(recording_position_counter_id)?;
                out.i32(recording_synced_position_counter_id)
            }
            Event::RecordingStopped {
                recording_id,
                stop_position,
            } => {
                out.i64(recording_id)?;
                out.i64(stop_position)
            }
            Event::ReplayReady {
                correlation_id,
                replay_session_id,
                recording_id,
                start_position,
                session_id,
                stream_id,
            } => {
                out.i64(correlation_id)?;
                out.i64(replay_session_id)?;
                out.i64(recording_id)?;
                out.i64(start_position)?;
                out.i32(session_id)?;
                out.i32(stream_id)
            }
            Event::ReplayStopped {
                replay_session_id,
                position,
                reason,
            } => {
                out.i64(replay_session_id)?;
                out.i64(position)?;
                out.i32(reason)
            }
        }
    }
}

/// A string's length on the wire: its length field, its bytes and its padding to a
/// multiple of 4, or `None` when it is longer than [`MAX_STRING_LENGTH`].
const fn padded_string_length(bytes: usize) -> Option<usize> {
    if bytes > MAX_STRING_LENGTH {
        return None;
    }
    Some(4 + bytes.div_ceil(4) * 4)
}

/// Reads fields from the front of a message, in order, checking each against the bytes
/// left.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self.pos.saturating_add(n);
        match self.bytes.get(self.pos..end) {
            Some(slice) => {
                self.pos = end;
                Ok(slice)
            }
            None => Err(ProtocolError::TooShort {
                given: self.bytes.len(),
                needed: end,
            }),
        }
    }

    fn i32(&mut self) -> Result<i32, ProtocolError> {
        let bytes = self.take(4)?;
        let array: [u8; 4] = bytes.try_into().map_err(|_| ProtocolError::TooShort {
            given: self.bytes.len(),
            needed: self.pos,
        })?;
        Ok(i32::from_le_bytes(array))
    }

    fn i64(&mut self) -> Result<i64, ProtocolError> {
        let bytes = self.take(8)?;
        let array: [u8; 8] = bytes.try_into().map_err(|_| ProtocolError::TooShort {
            given: self.bytes.len(),
            needed: self.pos,
        })?;
        Ok(i64::from_le_bytes(array))
    }

    /// A string: its length, then its bytes, then the padding to a multiple of 4. The
    /// length field itself missing is `too-short`; a length out of bounds, or bytes or
    /// padding past the message, is `string`.
    fn string(&mut self) -> Result<&'a [u8], ProtocolError> {
        let length = self.i32()?;
        let available = self.bytes.len().saturating_sub(self.pos);
        let error = ProtocolError::String {
            length: i64::from(length),
            available,
        };
        let bytes = usize::try_from(length).map_err(|_| error)?;
        let padded = padded_string_length(bytes).ok_or(error)?.saturating_sub(4);
        if padded > available {
            return Err(error);
        }
        let string = self.take(bytes)?;
        self.take(padded.saturating_sub(bytes))?;
        Ok(string)
    }

    /// The message ends where the fields end, or it is `length`.
    const fn finish(self) -> Result<(), ProtocolError> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(ProtocolError::Length {
                given: self.bytes.len(),
                expected: self.pos,
            })
        }
    }
}

/// Where an encoder puts fields: a counter of bytes, or a buffer.
trait Sink {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), ProtocolError>;

    fn i32(&mut self, value: i32) -> Result<(), ProtocolError> {
        self.bytes(&value.to_le_bytes())
    }

    fn i64(&mut self, value: i64) -> Result<(), ProtocolError> {
        self.bytes(&value.to_le_bytes())
    }

    fn string(&mut self, string: &[u8]) -> Result<(), ProtocolError> {
        let Some(padded) = padded_string_length(string.len()) else {
            return Err(ProtocolError::String {
                length: i64::try_from(string.len()).unwrap_or(i64::MAX),
                available: MAX_STRING_LENGTH,
            });
        };
        let length = i32::try_from(string.len()).unwrap_or(i32::MAX);
        self.i32(length)?;
        self.bytes(string)?;
        let padding = padded.saturating_sub(4).saturating_sub(string.len());
        self.bytes(&[0u8; 3][..padding.min(3)])
    }
}

/// Counts the bytes a message takes.
struct Measure(usize);

impl Sink for Measure {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(())
    }
}

/// Writes into a buffer that [`Command::write`] or [`Event::write`] has measured.
struct Writer<'b> {
    buffer: &'b mut [u8],
    pos: usize,
}

impl Sink for Writer<'_> {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        let end = self.pos.saturating_add(bytes.len());
        match self.buffer.get_mut(self.pos..end) {
            Some(slot) => {
                slot.copy_from_slice(bytes);
                self.pos = end;
                Ok(())
            }
            None => Err(ProtocolError::TooShort {
                given: self.buffer.len(),
                needed: end,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_pad_to_four_and_stop_at_the_bound() {
        assert_eq!(padded_string_length(0), Some(4));
        assert_eq!(padded_string_length(1), Some(8));
        assert_eq!(padded_string_length(4), Some(8));
        assert_eq!(padded_string_length(11), Some(16));
        assert_eq!(
            padded_string_length(MAX_STRING_LENGTH),
            Some(4 + MAX_STRING_LENGTH)
        );
        assert_eq!(padded_string_length(MAX_STRING_LENGTH + 1), None);
    }

    #[test]
    fn a_write_into_a_short_buffer_writes_nothing() {
        let command = Command::ClientClose {
            client_id: 5,
            correlation_id: 10,
        };
        let mut buffer = [0xAAu8; 15];
        assert_eq!(
            command.write(&mut buffer),
            Err(ProtocolError::TooShort {
                given: 15,
                needed: 16
            })
        );
        assert!(buffer.iter().all(|&b| b == 0xAA));
    }

    #[test]
    fn a_string_past_the_bound_is_refused_before_writing() {
        let long = [b'c'; MAX_STRING_LENGTH + 1];
        let event = Event::Error {
            offending_correlation_id: 1,
            error_code: ErrorCode::GenericError.code(),
            error_message: &long,
        };
        assert_eq!(
            event.encoded_length(),
            Err(ProtocolError::String {
                length: MAX_STRING_LENGTH as i64 + 1,
                available: MAX_STRING_LENGTH
            })
        );
    }

    #[test]
    fn the_error_code_follows_the_spec_order() {
        assert_eq!(
            ProtocolError::TooShort {
                given: 8,
                needed: 16
            }
            .error_code(),
            None
        );
        assert_eq!(
            ProtocolError::TooShort {
                given: 20,
                needed: 24
            }
            .error_code(),
            Some(ErrorCode::MalformedCommand)
        );
        assert_eq!(
            ProtocolError::Type { found: 7 }.error_code(),
            Some(ErrorCode::UnknownCommandType)
        );
        assert_eq!(
            ProtocolError::Length {
                given: 20,
                expected: 16
            }
            .error_code(),
            Some(ErrorCode::MalformedCommand)
        );
        assert_eq!(
            ErrorCode::from_code(10),
            Some(ErrorCode::ResourceTemporarilyUnavailable)
        );
        assert_eq!(ErrorCode::from_code(4), None);
        assert_eq!(
            ErrorCode::from_code(19),
            None,
            "left free for Aeron's later codes"
        );
        assert_eq!(ErrorCode::from_code(22), Some(ErrorCode::AlreadyRecording));
        assert_eq!(ErrorCode::AlreadyRecording.code(), 22);
        assert_eq!(
            ErrorCode::from_code(23),
            Some(ErrorCode::PositionOutOfRange)
        );
        assert_eq!(ErrorCode::from_code(24), None);
    }

    #[test]
    fn a_replay_stop_reason_round_trips_and_stops_at_three() {
        for (code, reason) in [
            (0, ReplayStopReason::Ended),
            (1, ReplayStopReason::Stopped),
            (2, ReplayStopReason::NotConnected),
            (3, ReplayStopReason::Failed),
        ] {
            assert_eq!(ReplayStopReason::from_code(code), Some(reason));
            assert_eq!(reason.code(), code);
        }
        assert_eq!(ReplayStopReason::from_code(4), None);
        assert_eq!(ReplayStopReason::from_code(-1), None);
    }

    #[test]
    fn the_replay_conventions_are_aerons() {
        assert_eq!(NULL_POSITION, -1);
        assert_eq!(NULL_LENGTH, -1);
        assert_eq!(FOLLOW_LENGTH, i64::MAX);
        let command = Command::StartReplay {
            client_id: 5,
            correlation_id: 13,
            recording_id: 0,
            position: NULL_POSITION,
            length: NULL_LENGTH,
            replay_stream_id: 2001,
            replay_channel: IPC_CHANNEL,
        };
        // 16 bytes of ids, three i64s, an i32 and the padded channel.
        assert_eq!(command.encoded_length(), Ok(16 + 24 + 4 + 16));
    }

    #[test]
    fn every_error_names_the_docs_anchor() {
        extern crate std;
        use std::string::ToString;
        for error in [
            ProtocolError::TooShort {
                given: 1,
                needed: 2,
            },
            ProtocolError::Type { found: 3 },
            ProtocolError::String {
                length: -1,
                available: 4,
            },
            ProtocolError::Length {
                given: 5,
                expected: 4,
            },
        ] {
            let text = error.to_string();
            assert!(
                text.starts_with(&std::format!("message rejected ({})", error.reason())),
                "{text}"
            );
            assert!(text.ends_with(DOC), "{text}");
        }
    }
}
