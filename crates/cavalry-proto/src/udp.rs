//! The UDP transport's frames, datagram and arithmetic: `spec/udp.md`, version 0
//! (ADR 0040).
//!
//! [`parse`] reads one control frame, or recognises a datagram of data frames, from the
//! front of a byte range and runs the spec's checks in the spec's order;
//! [`Datagram::check`] walks a datagram of data frames the same way. Each frame type
//! encodes itself with `to_bytes`. The layouts are private structs whose fields are
//! little-endian wire types, so each struct is its table in the spec and the compiler
//! checks it (ADR 0009). The constants under "Values" are the driver's in this version.

use core::fmt;
use core::time::Duration;

use zerocopy::little_endian::{I32, I64, U16};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::frame::{DataFrameHeader, FRAME_ALIGNMENT, FrameError, HEADER_LENGTH};

/// The version of `spec/udp.md` this crate reads and writes: the version byte of every
/// frame on the wire.
pub const VERSION: i8 = 0;

/// The docs anchor every error message names.
const DOC: &str = "docs/reference/udp.md#what-a-reader-rejects";

/// Length of a setup frame in bytes.
pub const SETUP_LENGTH: usize = 40;
/// Length of a status message without its group tag.
pub const STATUS_MESSAGE_LENGTH: usize = 36;
/// Length of a status message with its group tag.
pub const STATUS_MESSAGE_TAGGED_LENGTH: usize = 44;
/// Length of a NAK.
pub const NAK_LENGTH: usize = 28;
/// Length of an RTT measurement.
pub const RTT_MEASUREMENT_LENGTH: usize = 40;
/// Length of a heartbeat: a data frame header whose frame length is 0.
pub const HEARTBEAT_LENGTH: usize = HEADER_LENGTH;

/// The smallest term length a setup frame may name: `spec/log-buffer.md`.
pub const MIN_TERM_LENGTH: u32 = 64 * 1024;
/// The largest term length a setup frame may name.
pub const MAX_TERM_LENGTH: u32 = 1024 * 1024 * 1024;
/// The smallest MTU length a setup frame may name.
pub const MIN_MTU_LENGTH: u32 = 64;
/// The largest MTU length: the largest UDP payload rounded down to the frame alignment.
pub const MAX_MTU_LENGTH: u32 = 65_504;
/// The longest message any term length allows, 16 MiB (`spec/log-buffer.md`).
pub const MAX_MESSAGE_LENGTH_CAP: u32 = 16 * 1024 * 1024;

/// A setup frame's flag bit 7, Aeron's send response setup: reserved in this version.
pub const SETUP_SEND_RESPONSE: u8 = 0x80;
/// A setup frame's flag bit 6, Aeron's group: reserved in this version.
pub const SETUP_GROUP: u8 = 0x40;
/// A status message's flag bit 7: the receiver asks for a setup frame.
pub const STATUS_MESSAGE_SEND_SETUP: u8 = 0x80;
/// A status message's flag bit 6: the receiver has consumed the stream to its end.
pub const STATUS_MESSAGE_END_OF_STREAM: u8 = 0x40;
/// An RTT measurement's flag bit 7: a probe that asks for an answer.
pub const RTT_MEASUREMENT_REPLY: u8 = 0x80;

// ---- Values: `spec/udp.md`, "Values" -------------------------------------------------

/// The MTU length of a `cavalry:udp` publication's log: the largest datagram.
pub const MTU_LENGTH_DEFAULT: u32 = 1408;
/// The term length of a `cavalry:udp` publication's log.
pub const UDP_TERM_LENGTH: u32 = 16 * 1024 * 1024;
/// The receiver window length an image asks for, capped at half its term by
/// [`receiver_window_length`].
pub const RECEIVER_WINDOW_LENGTH: u32 = 128 * 1024;
/// The receive buffer the driver asks of a receive endpoint's socket.
pub const SOCKET_RCVBUF_LENGTH: u32 = 128 * 1024;
/// How often the sender sends a setup frame until a status message arrives.
pub const SETUP_INTERVAL: Duration = Duration::from_millis(100);
/// How long without a data frame sent before the sender sends a heartbeat.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(100);
/// How long without a status message sent before the receiver sends one.
pub const STATUS_MESSAGE_INTERVAL: Duration = Duration::from_millis(200);
/// How long after a gap is first seen before the receiver sends a NAK for it.
pub const NAK_DELAY: Duration = Duration::from_micros(1);
/// How often the receiver sends the NAK again while the same gap persists.
pub const NAK_RETRY: Duration = Duration::from_micros(100);
/// How long a sender's retransmit stays outstanding after it was sent.
pub const RETRANSMIT_LINGER: Duration = Duration::from_millis(10);
/// How long without a status message before a publication is not connected.
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
/// How long without an accepted frame before an image is retired.
pub const IMAGE_LIVENESS_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a closed publication lingers to answer NAKs.
pub const LINGER: Duration = Duration::from_secs(5);
/// How many datagrams one burst of the sender sends at most, per publication and cycle.
pub const DATAGRAMS_PER_BURST: usize = 16;
/// How many datagrams the receiver drains from a socket at most, per poll.
pub const DATAGRAMS_PER_POLL: usize = 16;
/// How many send endpoints a driver holds; a 65th is error code 10.
pub const MAX_SEND_ENDPOINTS: usize = 64;
/// How many receive endpoints a driver holds; a 65th is error code 10.
pub const MAX_RECEIVE_ENDPOINTS: usize = 64;

/// Why a reader rejected a frame or a datagram. The variants are the spec's checks in
/// the order they run, and [`UdpError::reason`] is the name the spec and the vectors
/// use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UdpError {
    /// Fewer bytes than the frame's type needs were given.
    TooShort {
        /// How many bytes were given.
        given: usize,
        /// How many the type needs.
        needed: usize,
    },
    /// The version byte is not [`VERSION`].
    Version {
        /// The version byte found.
        found: i8,
    },
    /// The type is one this reader does not handle.
    Type {
        /// The type found.
        found: u16,
    },
    /// The frame length is not the type's length, a status message's is neither 36 nor
    /// 44, a heartbeat is not exactly 32 bytes, or a datagram of data frames is not a
    /// multiple of 32 bytes long.
    Length {
        /// The length found.
        found: i64,
        /// The length, or the alignment, the spec asks for.
        expected: usize,
    },
    /// A setup frame's term length is not a power of two from [`MIN_TERM_LENGTH`] to
    /// [`MAX_TERM_LENGTH`].
    TermLength {
        /// The term length found.
        found: i32,
    },
    /// A setup frame's MTU length is not a multiple of 32 from [`MIN_MTU_LENGTH`] to
    /// [`MAX_MTU_LENGTH`].
    Mtu {
        /// The MTU length found.
        found: i32,
    },
    /// A term offset is negative, not a multiple of 32, or not below the term length.
    TermOffset {
        /// The term offset found.
        found: i32,
        /// The term length it was checked against.
        term_length: u32,
    },
    /// A status message's receiver window length is negative.
    Window {
        /// The window found.
        found: i32,
    },
    /// A NAK's length is not a positive multiple of 32, or runs past the end of the term.
    NakLength {
        /// The length found.
        found: i32,
        /// The NAK's term offset.
        term_offset: i32,
        /// The term length it was checked against.
        term_length: u32,
    },
    /// A datagram of data frames failed a check: a frame failed `spec/frame.md`'s checks
    /// against the bytes left, a frame's term offset is not the running offset from the
    /// first, or the frames run past the end of the term.
    Datagram {
        /// The byte offset within the datagram of the frame that failed.
        at: usize,
        /// Which check failed.
        cause: DatagramCause,
    },
}

/// Which check a datagram of data frames failed: [`UdpError::Datagram`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DatagramCause {
    /// The frame failed `spec/frame.md`'s checks.
    Frame(FrameError),
    /// The frame's term offset is not the running offset from the first frame's.
    TermOffset {
        /// The term offset found.
        found: i32,
        /// The running offset expected.
        expected: i32,
    },
    /// The frame would end past the end of the term.
    PastTerm {
        /// Where the frame would end, as a term offset.
        end: u64,
        /// The term length.
        term_length: u32,
    },
}

impl UdpError {
    /// The reason's name in `spec/udp.md`, "What a reader rejects".
    pub const fn reason(self) -> &'static str {
        match self {
            UdpError::TooShort { .. } => "too-short",
            UdpError::Version { .. } => "version",
            UdpError::Type { .. } => "type",
            UdpError::Length { .. } => "length",
            UdpError::TermLength { .. } => "term-length",
            UdpError::Mtu { .. } => "mtu",
            UdpError::TermOffset { .. } => "term-offset",
            UdpError::Window { .. } => "window",
            UdpError::NakLength { .. } => "nak-length",
            UdpError::Datagram { .. } => "datagram",
        }
    }
}

impl fmt::Display for UdpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            UdpError::TooShort { given, needed } => write!(
                f,
                "frame rejected (too-short): {given} bytes given, the frame needs {needed}; \
                 the datagram is truncated or is not a frame; see {DOC}"
            ),
            UdpError::Version { found } => write!(
                f,
                "frame rejected (version): version {found}, this reader knows {VERSION}; \
                 the peer runs another Cavalry version, upgrade the older side; see {DOC}"
            ),
            UdpError::Type { found } => write!(
                f,
                "frame rejected (type): type {found} is not one this reader handles; \
                 the peer runs another Cavalry version, upgrade the older side; see {DOC}"
            ),
            UdpError::Length { found, expected } => write!(
                f,
                "frame rejected (length): length {found} where the spec asks {expected}; \
                 the datagram is truncated or its length is corrupt; see {DOC}"
            ),
            UdpError::TermLength { found } => write!(
                f,
                "frame rejected (term-length): term length {found} is not a power of two \
                 from {MIN_TERM_LENGTH} to {MAX_TERM_LENGTH}; the sender's log is not one \
                 this reader can image; see {DOC}"
            ),
            UdpError::Mtu { found } => write!(
                f,
                "frame rejected (mtu): MTU length {found} is not a multiple of 32 from \
                 {MIN_MTU_LENGTH} to {MAX_MTU_LENGTH}; see {DOC}"
            ),
            UdpError::TermOffset { found, term_length } => write!(
                f,
                "frame rejected (term-offset): term offset {found} is not a multiple of 32 \
                 from 0 to below the term length {term_length}; see {DOC}"
            ),
            UdpError::Window { found } => write!(
                f,
                "frame rejected (window): receiver window length {found} is negative; \
                 see {DOC}"
            ),
            UdpError::NakLength {
                found,
                term_offset,
                term_length,
            } => write!(
                f,
                "frame rejected (nak-length): length {found} at term offset {term_offset} is \
                 not a positive multiple of 32 within the term length {term_length}; see {DOC}"
            ),
            UdpError::Datagram { at, cause } => match cause {
                DatagramCause::Frame(error) => write!(
                    f,
                    "datagram rejected (datagram): the frame at byte {at} failed a check: \
                     {error}"
                ),
                DatagramCause::TermOffset { found, expected } => write!(
                    f,
                    "datagram rejected (datagram): the frame at byte {at} carries term offset \
                     {found} where the frames before it end at {expected}; the datagram was \
                     not built from one term in order; see {DOC}"
                ),
                DatagramCause::PastTerm { end, term_length } => write!(
                    f,
                    "datagram rejected (datagram): the frame at byte {at} would end at term \
                     offset {end}, past the term length {term_length}; see {DOC}"
                ),
            },
        }
    }
}

impl core::error::Error for UdpError {}

// ---- the layouts -------------------------------------------------------------------

/// The first eight bytes of every frame: `spec/frame.md`'s first four fields.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawCommon {
    frame_length: I32,
    version: i8,
    flags: u8,
    frame_type: U16,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawSetup {
    common: RawCommon,
    term_offset: I32,
    session_id: I32,
    stream_id: I32,
    initial_term_id: I32,
    active_term_id: I32,
    term_length: I32,
    mtu_length: I32,
    ttl: I32,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawStatusMessage {
    common: RawCommon,
    session_id: I32,
    stream_id: I32,
    consumption_term_id: I32,
    consumption_term_offset: I32,
    receiver_window_length: I32,
    receiver_id: I64,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawNak {
    common: RawCommon,
    session_id: I32,
    stream_id: I32,
    term_id: I32,
    term_offset: I32,
    length: I32,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawRttMeasurement {
    common: RawCommon,
    session_id: I32,
    stream_id: I32,
    echo_timestamp: I64,
    reception_delta: I64,
    receiver_id: I64,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawHeartbeat {
    common: RawCommon,
    term_offset: I32,
    session_id: I32,
    stream_id: I32,
    term_id: I32,
    reserved_value: I64,
}

const _: () = assert!(core::mem::size_of::<RawCommon>() == 8);
const _: () = assert!(core::mem::size_of::<RawSetup>() == SETUP_LENGTH);
const _: () = assert!(core::mem::size_of::<RawStatusMessage>() == STATUS_MESSAGE_LENGTH);
const _: () = assert!(core::mem::size_of::<RawNak>() == NAK_LENGTH);
const _: () = assert!(core::mem::size_of::<RawRttMeasurement>() == RTT_MEASUREMENT_LENGTH);
const _: () = assert!(core::mem::size_of::<RawHeartbeat>() == HEARTBEAT_LENGTH);

/// The `type` field of a control frame: `spec/udp.md`, "The datagram".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum ControlType {
    /// A NAK: the receiver's request for a retransmission.
    Nak = 2,
    /// A status message: the receiver's position and window.
    StatusMessage = 3,
    /// A setup frame: the sender's announcement of a stream.
    Setup = 5,
    /// An RTT measurement: a round-trip probe or its answer.
    RttMeasurement = 6,
}

fn common(frame_type: ControlType, frame_length: usize, flags: u8) -> RawCommon {
    RawCommon {
        frame_length: I32::new(frame_length as i32),
        version: VERSION,
        flags,
        frame_type: U16::new(frame_type as u16),
    }
}

/// A setup frame: `spec/udp.md`, "Setup frame". The fields are the sender's; a reader
/// gets them from [`parse`], which checked them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SetupFrame {
    /// The flags byte: both named bits are reserved in this version.
    pub flags: u8,
    /// The term offset of the sender position.
    pub term_offset: i32,
    /// The publication's session id.
    pub session_id: i32,
    /// The publication's stream id.
    pub stream_id: i32,
    /// The log's initial term id.
    pub initial_term_id: i32,
    /// The term id of the sender position.
    pub active_term_id: i32,
    /// The log's term length.
    pub term_length: i32,
    /// The publication's MTU length.
    pub mtu_length: i32,
    /// Aeron's multicast time to live: 0 in this version.
    pub ttl: i32,
}

impl SetupFrame {
    /// The frame's bytes.
    pub fn to_bytes(&self) -> [u8; SETUP_LENGTH] {
        let raw = RawSetup {
            common: common(ControlType::Setup, SETUP_LENGTH, self.flags),
            term_offset: I32::new(self.term_offset),
            session_id: I32::new(self.session_id),
            stream_id: I32::new(self.stream_id),
            initial_term_id: I32::new(self.initial_term_id),
            active_term_id: I32::new(self.active_term_id),
            term_length: I32::new(self.term_length),
            mtu_length: I32::new(self.mtu_length),
            ttl: I32::new(self.ttl),
        };
        zerocopy::transmute!(raw)
    }

    /// The spec's checks on the fields after the common eight bytes, in order:
    /// `term-length`, `mtu`, `term-offset`.
    pub fn check(&self) -> Result<(), UdpError> {
        let term_length = u32::try_from(self.term_length).unwrap_or(0);
        if !(MIN_TERM_LENGTH..=MAX_TERM_LENGTH).contains(&term_length)
            || !term_length.is_power_of_two()
        {
            return Err(UdpError::TermLength {
                found: self.term_length,
            });
        }
        let mtu = u32::try_from(self.mtu_length).unwrap_or(0);
        if !(MIN_MTU_LENGTH..=MAX_MTU_LENGTH).contains(&mtu)
            || !mtu.is_multiple_of(FRAME_ALIGNMENT as u32)
        {
            return Err(UdpError::Mtu {
                found: self.mtu_length,
            });
        }
        check_term_offset(self.term_offset, term_length)
    }
}

/// A status message: `spec/udp.md`, "Status message".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StatusMessage {
    /// The flags byte: [`STATUS_MESSAGE_SEND_SETUP`] and [`STATUS_MESSAGE_END_OF_STREAM`].
    pub flags: u8,
    /// The image's session id.
    pub session_id: i32,
    /// The image's stream id.
    pub stream_id: i32,
    /// The term id of the consumption position.
    pub consumption_term_id: i32,
    /// The term offset of the consumption position.
    pub consumption_term_offset: i32,
    /// How far past the consumption position the sender may send.
    pub receiver_window_length: i32,
    /// The receiving driver's id.
    pub receiver_id: i64,
    /// Aeron's flow control group tag, present when the frame is 44 bytes; a receiver
    /// of this version sends none.
    pub group_tag: Option<i64>,
}

impl StatusMessage {
    /// The frame's length on the wire: 36, or 44 with a group tag.
    pub const fn frame_length(&self) -> usize {
        if self.group_tag.is_some() {
            STATUS_MESSAGE_TAGGED_LENGTH
        } else {
            STATUS_MESSAGE_LENGTH
        }
    }

    /// The frame's bytes, [`StatusMessage::frame_length`] of them, in a 44-byte array.
    pub fn to_bytes(&self) -> [u8; STATUS_MESSAGE_TAGGED_LENGTH] {
        let raw = RawStatusMessage {
            common: common(ControlType::StatusMessage, self.frame_length(), self.flags),
            session_id: I32::new(self.session_id),
            stream_id: I32::new(self.stream_id),
            consumption_term_id: I32::new(self.consumption_term_id),
            consumption_term_offset: I32::new(self.consumption_term_offset),
            receiver_window_length: I32::new(self.receiver_window_length),
            receiver_id: I64::new(self.receiver_id),
        };
        let mut out = [0u8; STATUS_MESSAGE_TAGGED_LENGTH];
        out[..STATUS_MESSAGE_LENGTH].copy_from_slice(raw.as_bytes());
        if let Some(tag) = self.group_tag {
            out[STATUS_MESSAGE_LENGTH..].copy_from_slice(&tag.to_le_bytes());
        }
        out
    }

    /// Whether the send-setup flag is set.
    pub const fn asks_for_setup(&self) -> bool {
        self.flags & STATUS_MESSAGE_SEND_SETUP != 0
    }

    /// Whether the end-of-stream flag is set.
    pub const fn is_end_of_stream(&self) -> bool {
        self.flags & STATUS_MESSAGE_END_OF_STREAM != 0
    }

    /// The spec's checks on the fields after the common eight bytes against the image's
    /// term length, in order: `term-offset`, `window`.
    pub fn check(&self, term_length: u32) -> Result<(), UdpError> {
        check_term_offset(self.consumption_term_offset, term_length)?;
        if self.receiver_window_length < 0 {
            return Err(UdpError::Window {
                found: self.receiver_window_length,
            });
        }
        Ok(())
    }
}

/// A NAK: `spec/udp.md`, "NAK".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nak {
    /// The flags byte: reserved, written 0 and carried as read.
    pub flags: u8,
    /// The image's session id.
    pub session_id: i32,
    /// The image's stream id.
    pub stream_id: i32,
    /// The term of the gap.
    pub term_id: i32,
    /// Where the gap begins.
    pub term_offset: i32,
    /// The gap's length.
    pub length: i32,
}

impl Nak {
    /// The frame's bytes.
    pub fn to_bytes(&self) -> [u8; NAK_LENGTH] {
        let raw = RawNak {
            common: common(ControlType::Nak, NAK_LENGTH, self.flags),
            session_id: I32::new(self.session_id),
            stream_id: I32::new(self.stream_id),
            term_id: I32::new(self.term_id),
            term_offset: I32::new(self.term_offset),
            length: I32::new(self.length),
        };
        zerocopy::transmute!(raw)
    }

    /// The spec's checks against the image's term length, in order: `term-offset`,
    /// `nak-length`.
    pub fn check(&self, term_length: u32) -> Result<(), UdpError> {
        check_term_offset(self.term_offset, term_length)?;
        let length = u32::try_from(self.length).unwrap_or(0);
        let end = u64::from(self.term_offset.unsigned_abs()) + u64::from(length);
        if length == 0
            || !length.is_multiple_of(FRAME_ALIGNMENT as u32)
            || end > u64::from(term_length)
        {
            return Err(UdpError::NakLength {
                found: self.length,
                term_offset: self.term_offset,
                term_length,
            });
        }
        Ok(())
    }
}

/// An RTT measurement: `spec/udp.md`, "RTT measurement".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RttMeasurement {
    /// The flags byte: [`RTT_MEASUREMENT_REPLY`] on a probe, clear on an answer.
    pub flags: u8,
    /// The stream's session id.
    pub session_id: i32,
    /// The stream's stream id.
    pub stream_id: i32,
    /// The prober's clock in nanoseconds, echoed by the answer.
    pub echo_timestamp: i64,
    /// The nanoseconds the answering side held the probe: 0 in this version.
    pub reception_delta: i64,
    /// The receiver the probe is for, 0 for any, echoed by the answer.
    pub receiver_id: i64,
}

impl RttMeasurement {
    /// The frame's bytes.
    pub fn to_bytes(&self) -> [u8; RTT_MEASUREMENT_LENGTH] {
        let raw = RawRttMeasurement {
            common: common(
                ControlType::RttMeasurement,
                RTT_MEASUREMENT_LENGTH,
                self.flags,
            ),
            session_id: I32::new(self.session_id),
            stream_id: I32::new(self.stream_id),
            echo_timestamp: I64::new(self.echo_timestamp),
            reception_delta: I64::new(self.reception_delta),
            receiver_id: I64::new(self.receiver_id),
        };
        zerocopy::transmute!(raw)
    }

    /// Whether this is a probe that asks for an answer.
    pub const fn is_probe(&self) -> bool {
        self.flags & RTT_MEASUREMENT_REPLY != 0
    }

    /// The answer to this probe: the same frame without the reply flag and a reception
    /// delta of 0 (`spec/udp.md`, "The sender", step 5).
    pub const fn answer(&self) -> RttMeasurement {
        RttMeasurement {
            flags: self.flags & !RTT_MEASUREMENT_REPLY,
            session_id: self.session_id,
            stream_id: self.stream_id,
            echo_timestamp: self.echo_timestamp,
            reception_delta: 0,
            receiver_id: self.receiver_id,
        }
    }
}

/// A heartbeat: `spec/udp.md`, "Heartbeat". A data frame header whose frame length is
/// 0, carrying the sender position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Heartbeat {
    /// BEGIN and END, with EOS once the publication is closed (`spec/frame.md`, "Flags").
    pub flags: u8,
    /// The term offset of the sender position.
    pub term_offset: i32,
    /// The publication's session id.
    pub session_id: i32,
    /// The publication's stream id.
    pub stream_id: i32,
    /// The term id of the sender position.
    pub term_id: i32,
    /// Written 0.
    pub reserved_value: i64,
}

impl Heartbeat {
    /// The frame's bytes.
    pub fn to_bytes(&self) -> [u8; HEARTBEAT_LENGTH] {
        let raw = RawHeartbeat {
            common: RawCommon {
                frame_length: I32::new(0),
                version: VERSION,
                flags: self.flags,
                frame_type: U16::new(crate::frame::FrameType::Data as u16),
            },
            term_offset: I32::new(self.term_offset),
            session_id: I32::new(self.session_id),
            stream_id: I32::new(self.stream_id),
            term_id: I32::new(self.term_id),
            reserved_value: I64::new(self.reserved_value),
        };
        zerocopy::transmute!(raw)
    }

    /// Whether the EOS flag is set: the publication is closed.
    pub const fn is_end_of_stream(&self) -> bool {
        self.flags & crate::frame::Flags::EOS.bits() != 0
    }
}

/// What [`parse`] found at the front of a datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlFrame {
    /// A setup frame.
    Setup(SetupFrame),
    /// A status message.
    StatusMessage(StatusMessage),
    /// A NAK.
    Nak(Nak),
    /// An RTT measurement.
    RttMeasurement(RttMeasurement),
    /// A heartbeat.
    Heartbeat(Heartbeat),
    /// A data or padding frame with a frame length above 0: the datagram holds data
    /// frames, which [`Datagram::check`] walks.
    Data,
}

fn check_term_offset(term_offset: i32, term_length: u32) -> Result<(), UdpError> {
    let in_term = u32::try_from(term_offset)
        .is_ok_and(|offset| offset < term_length && offset.is_multiple_of(FRAME_ALIGNMENT as u32));
    if !in_term {
        return Err(UdpError::TermOffset {
            found: term_offset,
            term_length,
        });
    }
    Ok(())
}

/// Reads one frame from the front of `bytes` and runs the spec's checks in the spec's
/// order against `term_length`, the image's term length for a status message, a NAK or
/// a heartbeat, which a setup frame does not need since it carries its own. A data or
/// padding frame with a frame length above 0 is [`ControlFrame::Data`], unchecked here:
/// [`Datagram::check`] is its check. A heartbeat can arrive for a session the receiver
/// has no record of, before any image (`spec/udp.md`, "The receiver", step 1); the
/// receiver passes [`MAX_TERM_LENGTH`] there, so that only the frame's shape is checked
/// and the offset is held against the image's term length once it has one (card 181).
pub fn parse(bytes: &[u8], term_length: u32) -> Result<ControlFrame, UdpError> {
    let Ok((head, _)) = RawCommon::ref_from_prefix(bytes) else {
        return Err(UdpError::TooShort {
            given: bytes.len(),
            needed: 8,
        });
    };
    if head.version != VERSION {
        return Err(UdpError::Version {
            found: head.version,
        });
    }
    let frame_length = head.frame_length.get();
    match head.frame_type.get() {
        0 | 1 => {
            if frame_length != 0 {
                return Ok(ControlFrame::Data);
            }
            if head.frame_type.get() == 0 {
                // A padding frame is at least a header long; only a data frame may be
                // a heartbeat (`spec/udp.md`, "Heartbeat").
                return Err(UdpError::Length {
                    found: 0,
                    expected: HEADER_LENGTH,
                });
            }
            if bytes.len() < HEARTBEAT_LENGTH {
                return Err(UdpError::TooShort {
                    given: bytes.len(),
                    needed: HEARTBEAT_LENGTH,
                });
            }
            if bytes.len() != HEARTBEAT_LENGTH {
                return Err(UdpError::Length {
                    found: bytes.len() as i64,
                    expected: HEARTBEAT_LENGTH,
                });
            }
            let raw = RawHeartbeat::ref_from_bytes(&bytes[..HEARTBEAT_LENGTH]).map_err(|_| {
                UdpError::TooShort {
                    given: bytes.len(),
                    needed: HEARTBEAT_LENGTH,
                }
            })?;
            let heartbeat = Heartbeat {
                flags: raw.common.flags,
                term_offset: raw.term_offset.get(),
                session_id: raw.session_id.get(),
                stream_id: raw.stream_id.get(),
                term_id: raw.term_id.get(),
                reserved_value: raw.reserved_value.get(),
            };
            check_term_offset(heartbeat.term_offset, term_length)?;
            Ok(ControlFrame::Heartbeat(heartbeat))
        }
        2 => {
            let raw = exact::<RawNak>(bytes, NAK_LENGTH, frame_length)?;
            let nak = Nak {
                flags: raw.common.flags,
                session_id: raw.session_id.get(),
                stream_id: raw.stream_id.get(),
                term_id: raw.term_id.get(),
                term_offset: raw.term_offset.get(),
                length: raw.length.get(),
            };
            nak.check(term_length)?;
            Ok(ControlFrame::Nak(nak))
        }
        3 => {
            if bytes.len() < STATUS_MESSAGE_LENGTH {
                return Err(UdpError::TooShort {
                    given: bytes.len(),
                    needed: STATUS_MESSAGE_LENGTH,
                });
            }
            let tagged = match usize::try_from(frame_length) {
                Ok(STATUS_MESSAGE_LENGTH) => false,
                Ok(STATUS_MESSAGE_TAGGED_LENGTH) if bytes.len() >= STATUS_MESSAGE_TAGGED_LENGTH => {
                    true
                }
                _ => {
                    return Err(UdpError::Length {
                        found: i64::from(frame_length),
                        expected: STATUS_MESSAGE_LENGTH,
                    });
                }
            };
            let raw = RawStatusMessage::ref_from_bytes(&bytes[..STATUS_MESSAGE_LENGTH]).map_err(
                |_| UdpError::TooShort {
                    given: bytes.len(),
                    needed: STATUS_MESSAGE_LENGTH,
                },
            )?;
            let group_tag = if tagged {
                let mut tag = [0u8; 8];
                tag.copy_from_slice(&bytes[STATUS_MESSAGE_LENGTH..STATUS_MESSAGE_TAGGED_LENGTH]);
                Some(i64::from_le_bytes(tag))
            } else {
                None
            };
            let message = StatusMessage {
                flags: raw.common.flags,
                session_id: raw.session_id.get(),
                stream_id: raw.stream_id.get(),
                consumption_term_id: raw.consumption_term_id.get(),
                consumption_term_offset: raw.consumption_term_offset.get(),
                receiver_window_length: raw.receiver_window_length.get(),
                receiver_id: raw.receiver_id.get(),
                group_tag,
            };
            message.check(term_length)?;
            Ok(ControlFrame::StatusMessage(message))
        }
        5 => {
            let raw = exact::<RawSetup>(bytes, SETUP_LENGTH, frame_length)?;
            let setup = SetupFrame {
                flags: raw.common.flags,
                term_offset: raw.term_offset.get(),
                session_id: raw.session_id.get(),
                stream_id: raw.stream_id.get(),
                initial_term_id: raw.initial_term_id.get(),
                active_term_id: raw.active_term_id.get(),
                term_length: raw.term_length.get(),
                mtu_length: raw.mtu_length.get(),
                ttl: raw.ttl.get(),
            };
            setup.check()?;
            Ok(ControlFrame::Setup(setup))
        }
        6 => {
            let raw = exact::<RawRttMeasurement>(bytes, RTT_MEASUREMENT_LENGTH, frame_length)?;
            Ok(ControlFrame::RttMeasurement(RttMeasurement {
                flags: raw.common.flags,
                session_id: raw.session_id.get(),
                stream_id: raw.stream_id.get(),
                echo_timestamp: raw.echo_timestamp.get(),
                reception_delta: raw.reception_delta.get(),
                receiver_id: raw.receiver_id.get(),
            }))
        }
        found => Err(UdpError::Type { found }),
    }
}

/// The `too-short` and `length` checks for a frame of one fixed length, then its bytes.
fn exact<T: FromBytes + KnownLayout + Immutable>(
    bytes: &[u8],
    needed: usize,
    frame_length: i32,
) -> Result<&T, UdpError> {
    if bytes.len() < needed {
        return Err(UdpError::TooShort {
            given: bytes.len(),
            needed,
        });
    }
    if usize::try_from(frame_length) != Ok(needed) {
        return Err(UdpError::Length {
            found: i64::from(frame_length),
            expected: needed,
        });
    }
    T::ref_from_bytes(&bytes[..needed]).map_err(|_| UdpError::TooShort {
        given: bytes.len(),
        needed,
    })
}

/// A datagram of data and padding frames from one term: `spec/udp.md`, "The datagram".
/// [`Datagram::check`] runs the whole datagram's checks before anything is read from
/// it; [`Datagram::frames`] then walks it.
#[derive(Clone, Copy, Debug)]
pub struct Datagram<'a> {
    bytes: &'a [u8],
    term_length: u32,
    first_term_offset: u32,
    end_term_offset: u32,
    frames: usize,
}

impl<'a> Datagram<'a> {
    /// Checks `bytes` as a datagram of data frames against `term_length`, in the spec's
    /// order: `length` (a multiple of 32), then for each frame `spec/frame.md`'s checks
    /// against the bytes left, `term-offset` for the first frame's, and `datagram` for
    /// a term offset that is not the running one or a frame past the end of the term.
    pub fn check(bytes: &'a [u8], term_length: u32) -> Result<Self, UdpError> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(FRAME_ALIGNMENT) {
            return Err(UdpError::Length {
                found: bytes.len() as i64,
                expected: FRAME_ALIGNMENT,
            });
        }
        let mut at = 0usize;
        let mut first: Option<u32> = None;
        let mut running: u32 = 0;
        let mut frames = 0usize;
        while at < bytes.len() {
            let available = bytes.len() - at;
            let header =
                DataFrameHeader::parse_within(&bytes[at..], available).map_err(|error| {
                    UdpError::Datagram {
                        at,
                        cause: DatagramCause::Frame(error),
                    }
                })?;
            let term_offset = header.term_offset();
            match first {
                None => {
                    check_term_offset(term_offset, term_length)?;
                    // Checked above: non-negative and below the term length.
                    let offset = term_offset as u32;
                    first = Some(offset);
                    running = offset;
                }
                Some(_) => {
                    if u32::try_from(term_offset) != Ok(running) {
                        return Err(UdpError::Datagram {
                            at,
                            cause: DatagramCause::TermOffset {
                                found: term_offset,
                                expected: running as i32,
                            },
                        });
                    }
                }
            }
            let aligned = header.aligned_length();
            let end = u64::from(running) + aligned as u64;
            if end > u64::from(term_length) {
                return Err(UdpError::Datagram {
                    at,
                    cause: DatagramCause::PastTerm { end, term_length },
                });
            }
            running = end as u32;
            at += aligned;
            frames += 1;
        }
        Ok(Self {
            bytes,
            term_length,
            first_term_offset: first.unwrap_or(0),
            end_term_offset: running,
            frames,
        })
    }

    /// The term offset of the first frame.
    pub const fn first_term_offset(&self) -> u32 {
        self.first_term_offset
    }

    /// The term offset just past the last frame's alignment padding.
    pub const fn end_term_offset(&self) -> u32 {
        self.end_term_offset
    }

    /// How many bytes of the term the datagram covers, alignment padding included.
    pub const fn length(&self) -> u32 {
        self.end_term_offset - self.first_term_offset
    }

    /// How many frames the datagram holds.
    pub const fn frame_count(&self) -> usize {
        self.frames
    }

    /// The term length the datagram was checked against.
    pub const fn term_length(&self) -> u32 {
        self.term_length
    }

    /// The datagram's bytes.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// The frames in order, each with its bytes, header and alignment padding included.
    pub fn frames(&self) -> DatagramFrames<'a> {
        DatagramFrames {
            bytes: self.bytes,
            at: 0,
        }
    }
}

/// One frame of a checked [`Datagram`]: its header and its bytes, alignment padding
/// included, as [`Datagram::frames`] yields them.
#[derive(Clone, Copy, Debug)]
pub struct DatagramFrame<'a> {
    /// The frame's header, checked.
    pub header: DataFrameHeader,
    /// The frame's bytes from its header to the end of its alignment padding.
    pub bytes: &'a [u8],
}

/// The frames of a checked [`Datagram`], in order.
#[derive(Clone, Copy, Debug)]
pub struct DatagramFrames<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Iterator for DatagramFrames<'a> {
    type Item = DatagramFrame<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.bytes.get(self.at..)?;
        if rest.is_empty() {
            return None;
        }
        // The datagram was checked, so the header parses and the frame fits; a failure
        // here ends the walk rather than panicking.
        let header = DataFrameHeader::parse_within(rest, rest.len()).ok()?;
        let aligned = header.aligned_length().min(rest.len());
        let bytes = &rest[..aligned];
        self.at += aligned;
        Some(DatagramFrame { header, bytes })
    }
}

// ---- arithmetic: `spec/udp.md`, "The sender" and "The receiver" --------------------

/// A position from a term id and term offset, as `spec/log-buffer.md`, "Positions",
/// computes it: the term count wraps in 32 bits.
pub const fn position(
    term_id: i32,
    term_offset: i32,
    initial_term_id: i32,
    term_length: u32,
) -> i64 {
    (term_id.wrapping_sub(initial_term_id) as i64)
        .wrapping_mul(term_length as i64)
        .wrapping_add(term_offset as i64)
}

/// The longest message a term length allows: the smaller of term length ÷ 8 and 16 MiB.
pub const fn max_message_length(term_length: u32) -> u32 {
    let eighth = term_length / 8;
    if eighth < MAX_MESSAGE_LENGTH_CAP {
        eighth
    } else {
        MAX_MESSAGE_LENGTH_CAP
    }
}

/// The receiver window length an image of a term length gets: the lesser of
/// [`RECEIVER_WINDOW_LENGTH`] and term length ÷ 2.
pub const fn receiver_window_length(term_length: u32) -> u32 {
    let half = term_length / 2;
    if half < RECEIVER_WINDOW_LENGTH {
        half
    } else {
        RECEIVER_WINDOW_LENGTH
    }
}

/// What a status message does to the sender limit: [`sender_limit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SenderLimit {
    /// The consumption position the status message names.
    pub position: i64,
    /// Whether the position lies within the window around the sender position.
    pub accepted: bool,
    /// The sender limit after the status message: raised when accepted, unchanged
    /// otherwise.
    pub sender_limit: i64,
}

/// The sender limit after a status message naming `consumption_term_id` and
/// `consumption_term_offset` with `receiver_window_length`, for a publication at
/// `sender_position` with the limit `sender_limit`: the greater of the limit and the
/// position plus the window, when the position lies within
/// `[sender position − term ÷ 2, sender position + term + term ÷ 2]`.
pub const fn sender_limit(
    sender_position: i64,
    sender_limit: i64,
    consumption_term_id: i32,
    consumption_term_offset: i32,
    receiver_window_length: i32,
    initial_term_id: i32,
    term_length: u32,
) -> SenderLimit {
    let position = position(
        consumption_term_id,
        consumption_term_offset,
        initial_term_id,
        term_length,
    );
    let half = (term_length / 2) as i64;
    let low = sender_position.wrapping_sub(half);
    let high = sender_position
        .wrapping_add(term_length as i64)
        .wrapping_add(half);
    let accepted = low <= position && position <= high;
    let raised = position.wrapping_add(receiver_window_length as i64);
    let limit = if !accepted {
        sender_limit
    } else if raised > sender_limit {
        raised
    } else {
        sender_limit
    };
    SenderLimit {
        position,
        accepted,
        sender_limit: limit,
    }
}

/// Whether a NAK's position may be answered: [`retransmit_window`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RetransmitWindow {
    /// The position the NAK names.
    pub resend_position: i64,
    /// The lowest position the sender still answers for: sender position − term ÷ 2
    /// − the longest message.
    pub bottom: i64,
    /// Whether the position lies in `[bottom, sender position)`.
    pub in_window: bool,
}

/// The retransmit window of a publication at `sender_position` for a NAK naming
/// `term_id` and `term_offset`.
pub const fn retransmit_window(
    sender_position: i64,
    term_id: i32,
    term_offset: i32,
    initial_term_id: i32,
    term_length: u32,
) -> RetransmitWindow {
    let resend_position = position(term_id, term_offset, initial_term_id, term_length);
    let bottom = sender_position
        .wrapping_sub((term_length / 2) as i64)
        .wrapping_sub(max_message_length(term_length) as i64);
    RetransmitWindow {
        resend_position,
        bottom,
        in_window: bottom <= resend_position && resend_position < sender_position,
    }
}

/// Whether a data datagram is accepted by an image: [`receiver_bounds`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReceiverBounds {
    /// The datagram begins before the last status message's position.
    pub under_run: bool,
    /// The datagram ends beyond that position plus half a term.
    pub over_run: bool,
    /// Neither: the datagram's frames are inserted.
    pub accepted: bool,
}

/// The receiver's bounds for a datagram of `payload_length` bytes at `packet_position`
/// when the last status message sent named `last_sm_position`.
pub const fn receiver_bounds(
    last_sm_position: i64,
    packet_position: i64,
    payload_length: i32,
    term_length: u32,
) -> ReceiverBounds {
    let under_run = packet_position < last_sm_position;
    let end = packet_position.wrapping_add(payload_length as i64);
    let over_run = end > last_sm_position.wrapping_add((term_length / 2) as i64);
    ReceiverBounds {
        under_run,
        over_run,
        accepted: !under_run && !over_run,
    }
}

/// How far the slowest subscriber must move past the last status message's position
/// before the receiver sends another: a quarter of the window.
pub const fn status_message_threshold(receiver_window_length: i32) -> i32 {
    receiver_window_length / 4
}

/// Where an image created from a setup frame starts: [`image_start`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageStart {
    /// The position of the setup frame's active term id and term offset.
    pub position: i64,
    /// The receiver window length the image gets.
    pub receiver_window_length: u32,
}

/// The start of an image for a setup frame naming `initial_term_id`, `active_term_id`,
/// `term_offset` and `term_length`.
pub const fn image_start(
    initial_term_id: i32,
    active_term_id: i32,
    term_offset: i32,
    term_length: u32,
) -> ImageStart {
    ImageStart {
        position: position(active_term_id, term_offset, initial_term_id, term_length),
        receiver_window_length: receiver_window_length(term_length),
    }
}
