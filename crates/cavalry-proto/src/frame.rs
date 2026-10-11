//! The data frame header: `spec/frame.md`, version 0 (ADR 0008).
//!
//! [`DataFrameHeader::parse`] reads one header from the front of a byte range and runs
//! the spec's four checks in the spec's order. [`DataFrameHeader::write`] puts one in
//! place. The layout is one private struct whose fields are little-endian wire types,
//! so the struct is the table in the spec and the compiler checks it (ADR 0009).

use core::fmt;

use zerocopy::little_endian::{I32, I64, U16};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Length of a data frame header in bytes: `spec/frame.md`, "Layout".
pub const HEADER_LENGTH: usize = 32;

/// Every frame begins at a multiple of this many bytes within its term or packet.
pub const FRAME_ALIGNMENT: usize = 32;

/// The version of `spec/frame.md` this crate reads and writes.
pub const VERSION: i8 = 0;

/// The docs anchor every error message names.
const DOC: &str = "docs/reference/frame.md#what-a-reader-rejects";

/// The `type` field of a frame header: which frame follows the first eight bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum FrameType {
    /// A padding frame: fills the end of a term or an abandoned claim. Carries nothing.
    Pad = 0,
    /// A data frame: carries one fragment of a message.
    Data = 1,
}

impl TryFrom<u16> for FrameType {
    type Error = FrameError;

    /// The type for a `type` field, or [`FrameError::Type`] for one this crate does not
    /// handle.
    fn try_from(value: u16) -> Result<Self, FrameError> {
        match value {
            0 => Ok(FrameType::Pad),
            1 => Ok(FrameType::Data),
            found => Err(FrameError::Type { found }),
        }
    }
}

/// The `flags` byte of a data frame header: `spec/frame.md`, "Flags".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Flags(u8);

impl Flags {
    /// No flag set: what a writer puts on a padding frame.
    pub const NONE: Flags = Flags(0);
    /// The first fragment of a message.
    pub const BEGIN: Flags = Flags(0x80);
    /// The last fragment of a message.
    pub const END: Flags = Flags(0x40);
    /// End of stream: the publication closed, and nothing follows on this session.
    pub const EOS: Flags = Flags(0x20);
    /// [`Flags::BEGIN`] and [`Flags::END`] together: the message is this one frame.
    pub const UNFRAGMENTED: Flags = Flags(0xC0);
    /// The five low bits: written as 0 by a version 0 writer, ignored by a reader.
    pub const RESERVED: Flags = Flags(0x1F);

    /// The flags for the byte on the wire, reserved bits included.
    pub const fn from_bits(bits: u8) -> Flags {
        Flags(bits)
    }

    /// The byte on the wire.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether every bit of `other` is set in `self`.
    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for Flags {
    type Output = Flags;

    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}

/// Why a reader rejected a frame. The variants are the spec's four checks in the order
/// they run, and [`FrameError::reason`] is the name the spec and the vectors use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameError {
    /// Fewer than [`HEADER_LENGTH`] bytes were given.
    TooShort {
        /// How many bytes were given.
        given: usize,
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
    /// The frame length is below [`HEADER_LENGTH`] or above what the bytes given, or
    /// the field, allow.
    Length {
        /// The frame length found, or asked for.
        found: i64,
        /// The largest frame length the bytes given, or the field, allow.
        limit: usize,
    },
}

impl FrameError {
    /// The reason's name in `spec/frame.md`, "What a reader rejects": `too-short`,
    /// `version`, `type` or `length`.
    pub const fn reason(self) -> &'static str {
        match self {
            FrameError::TooShort { .. } => "too-short",
            FrameError::Version { .. } => "version",
            FrameError::Type { .. } => "type",
            FrameError::Length { .. } => "length",
        }
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            FrameError::TooShort { given } => write!(
                f,
                "frame rejected (too-short): {given} bytes given, a header is {HEADER_LENGTH}; \
                 the bytes are truncated or are not a frame; see {DOC}"
            ),
            FrameError::Version { found } => write!(
                f,
                "frame rejected (version): version {found}, this reader knows {VERSION}; \
                 the writer runs another Cavalry version, upgrade the older side; see {DOC}"
            ),
            FrameError::Type { found } => write!(
                f,
                "frame rejected (type): type {found} is not one this reader handles; \
                 the writer runs another Cavalry version, upgrade the older side; see {DOC}"
            ),
            FrameError::Length { found, limit } => write!(
                f,
                "frame rejected (length): frame length {found} is not between \
                 {HEADER_LENGTH} and {limit}; the frame is truncated or its length is \
                 corrupt; see {DOC}"
            ),
        }
    }
}

impl core::error::Error for FrameError {}

/// The layout table of `spec/frame.md` as bytes: little-endian fields at the spec's
/// offsets, no padding, any alignment. Private: the public type below validates.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct Raw {
    frame_length: I32,
    version: i8,
    flags: u8,
    frame_type: U16,
    term_offset: I32,
    session_id: I32,
    stream_id: I32,
    term_id: I32,
    reserved_value: I64,
}

const _: () = assert!(core::mem::size_of::<Raw>() == HEADER_LENGTH);

/// The frame length field's lower bound, as the field's type.
const MIN_FRAME_LENGTH: i32 = HEADER_LENGTH as i32;

/// One data frame header, validated. It comes only from [`DataFrameHeader::parse`] or
/// [`DataFrameHeader::new`], so its version is [`VERSION`], its type is one this crate
/// handles, and its frame length is at least [`HEADER_LENGTH`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DataFrameHeader {
    frame_length: i32,
    flags: Flags,
    frame_type: FrameType,
    term_offset: i32,
    session_id: i32,
    stream_id: i32,
    term_id: i32,
    reserved_value: i64,
}

impl DataFrameHeader {
    /// A header of `frame_type` with `flags` and a frame length of `frame_length`
    /// bytes, header included, with term offset, the ids and the reserved value 0.
    /// Fails with [`FrameError::Length`] when `frame_length` is below
    /// [`HEADER_LENGTH`] or above `i32::MAX`.
    pub fn new(
        frame_type: FrameType,
        flags: Flags,
        frame_length: usize,
    ) -> Result<Self, FrameError> {
        let found = i64::try_from(frame_length).unwrap_or(i64::MAX);
        let limit = usize::try_from(i32::MAX).unwrap_or(usize::MAX);
        let length =
            i32::try_from(frame_length).map_err(|_| FrameError::Length { found, limit })?;
        if length < MIN_FRAME_LENGTH {
            return Err(FrameError::Length { found, limit });
        }
        Ok(Self {
            frame_length: length,
            flags,
            frame_type,
            term_offset: 0,
            session_id: 0,
            stream_id: 0,
            term_id: 0,
            reserved_value: 0,
        })
    }

    /// The same header with this term offset: the frame's byte offset from the start
    /// of its term, a multiple of [`FRAME_ALIGNMENT`] by the spec, which the log buffer
    /// enforces.
    #[must_use]
    pub const fn with_term_offset(mut self, term_offset: i32) -> Self {
        self.term_offset = term_offset;
        self
    }

    /// The same header with this session id.
    #[must_use]
    pub const fn with_session_id(mut self, session_id: i32) -> Self {
        self.session_id = session_id;
        self
    }

    /// The same header with this stream id.
    #[must_use]
    pub const fn with_stream_id(mut self, stream_id: i32) -> Self {
        self.stream_id = stream_id;
        self
    }

    /// The same header with this term id.
    #[must_use]
    pub const fn with_term_id(mut self, term_id: i32) -> Self {
        self.term_id = term_id;
        self
    }

    /// The same header with this reserved value.
    #[must_use]
    pub const fn with_reserved_value(mut self, reserved_value: i64) -> Self {
        self.reserved_value = reserved_value;
        self
    }

    /// Reads one header from the front of `bytes` and runs the spec's checks in the
    /// spec's order, reporting the first that fails. On success, `bytes` holds at least
    /// the whole frame: [`DataFrameHeader::frame_length`] bytes. Alignment padding after
    /// the frame need not be present.
    pub fn parse(bytes: &[u8]) -> Result<Self, FrameError> {
        Self::parse_within(bytes, bytes.len())
    }

    /// [`DataFrameHeader::parse`] for a caller that holds the header's bytes and knows
    /// how many bytes the frame may occupy: `available` is the number of bytes from the
    /// frame's start to the end of its term or packet, and the `length` check runs
    /// against it instead of `bytes.len()`. A log buffer reader loads a header through
    /// its own cells and passes the bytes left in the term (`spec/log-buffer.md`,
    /// "Reading"). `bytes` still needs [`HEADER_LENGTH`] bytes, or the error is
    /// [`FrameError::TooShort`].
    pub fn parse_within(bytes: &[u8], available: usize) -> Result<Self, FrameError> {
        let Ok((raw, _)) = Raw::ref_from_prefix(bytes) else {
            return Err(FrameError::TooShort { given: bytes.len() });
        };
        if raw.version != VERSION {
            return Err(FrameError::Version { found: raw.version });
        }
        let frame_type = FrameType::try_from(raw.frame_type.get())?;
        let frame_length = raw.frame_length.get();
        let fits = usize::try_from(frame_length).is_ok_and(|length| length <= available);
        if frame_length < MIN_FRAME_LENGTH || !fits {
            return Err(FrameError::Length {
                found: i64::from(frame_length),
                limit: available,
            });
        }
        Ok(Self {
            frame_length,
            flags: Flags(raw.flags),
            frame_type,
            term_offset: raw.term_offset.get(),
            session_id: raw.session_id.get(),
            stream_id: raw.stream_id.get(),
            term_id: raw.term_id.get(),
            reserved_value: raw.reserved_value.get(),
        })
    }

    /// Writes the header into the first [`HEADER_LENGTH`] bytes of `bytes`, in place.
    /// The body and the alignment padding are the caller's. Fails with
    /// [`FrameError::TooShort`] when `bytes` is shorter than a header.
    pub fn write(&self, bytes: &mut [u8]) -> Result<(), FrameError> {
        let Ok((raw, _)) = Raw::mut_from_prefix(bytes) else {
            return Err(FrameError::TooShort { given: bytes.len() });
        };
        *raw = self.raw();
        Ok(())
    }

    /// The header as its [`HEADER_LENGTH`] bytes on the wire.
    pub fn to_bytes(&self) -> [u8; HEADER_LENGTH] {
        zerocopy::transmute!(self.raw())
    }

    fn raw(&self) -> Raw {
        Raw {
            frame_length: I32::new(self.frame_length),
            version: VERSION,
            flags: self.flags.0,
            frame_type: U16::new(self.frame_type as u16),
            term_offset: I32::new(self.term_offset),
            session_id: I32::new(self.session_id),
            stream_id: I32::new(self.stream_id),
            term_id: I32::new(self.term_id),
            reserved_value: I64::new(self.reserved_value),
        }
    }

    /// Length of the frame in bytes, header included, before alignment padding. At
    /// least [`HEADER_LENGTH`].
    pub const fn frame_length(&self) -> i32 {
        self.frame_length
    }

    /// The version byte: always [`VERSION`] for a header this crate produced.
    pub const fn version(&self) -> i8 {
        VERSION
    }

    /// The flags byte, reserved bits included.
    pub const fn flags(&self) -> Flags {
        self.flags
    }

    /// The frame's type.
    pub const fn frame_type(&self) -> FrameType {
        self.frame_type
    }

    /// The frame's byte offset from the start of its term.
    pub const fn term_offset(&self) -> i32 {
        self.term_offset
    }

    /// The publication's session.
    pub const fn session_id(&self) -> i32 {
        self.session_id
    }

    /// The stream the frame belongs to.
    pub const fn stream_id(&self) -> i32 {
        self.stream_id
    }

    /// The term the frame is in.
    pub const fn term_id(&self) -> i32 {
        self.term_id
    }

    /// The value carried from publisher to subscriber unchanged.
    pub const fn reserved_value(&self) -> i64 {
        self.reserved_value
    }

    /// Length of the body in bytes: the frame length less the header.
    pub fn body_length(&self) -> usize {
        self.length().saturating_sub(HEADER_LENGTH)
    }

    /// The frame length rounded up to a multiple of [`FRAME_ALIGNMENT`]: where the next
    /// frame begins, relative to this one.
    pub fn aligned_length(&self) -> usize {
        self.length().div_ceil(FRAME_ALIGNMENT) * FRAME_ALIGNMENT
    }

    /// The frame length as a `usize`. The field is at least [`MIN_FRAME_LENGTH`] by
    /// construction, so the conversion cannot fail; 0 is never returned.
    fn length(&self) -> usize {
        usize::try_from(self.frame_length).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_combine_and_contain() {
        let flags = Flags::BEGIN | Flags::EOS;
        assert_eq!(flags.bits(), 0xA0);
        assert!(flags.contains(Flags::BEGIN));
        assert!(!flags.contains(Flags::END));
        assert!(Flags::UNFRAGMENTED.contains(Flags::BEGIN | Flags::END));
        assert!(Flags::from_bits(0xDF).contains(Flags::RESERVED));
    }

    #[test]
    fn lengths_round_up_to_the_alignment() {
        let header = |length| DataFrameHeader::new(FrameType::Data, Flags::NONE, length);
        assert_eq!(
            header(32).map(|h| (h.body_length(), h.aligned_length())),
            Ok((0, 32))
        );
        assert_eq!(
            header(33).map(|h| (h.body_length(), h.aligned_length())),
            Ok((1, 64))
        );
        assert_eq!(
            header(64).map(|h| (h.body_length(), h.aligned_length())),
            Ok((32, 64))
        );
        assert_eq!(
            header(31).map(|_| ()),
            Err(FrameError::Length {
                found: 31,
                limit: i32::MAX as usize
            })
        );
        assert_eq!(
            header(usize::MAX).map(|_| ()),
            Err(FrameError::Length {
                found: i64::MAX,
                limit: i32::MAX as usize
            })
        );
    }

    #[test]
    fn parse_within_bounds_the_length_by_the_bytes_available() {
        let header = DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, 64).unwrap();
        let bytes = header.to_bytes();
        assert_eq!(
            DataFrameHeader::parse(&bytes).map(|_| ()),
            Err(FrameError::Length {
                found: 64,
                limit: 32
            }),
            "the slice alone is too short for the frame"
        );
        assert_eq!(DataFrameHeader::parse_within(&bytes, 64), Ok(header));
        assert_eq!(DataFrameHeader::parse_within(&bytes, 4096), Ok(header));
        assert_eq!(
            DataFrameHeader::parse_within(&bytes, 63).map(|_| ()),
            Err(FrameError::Length {
                found: 64,
                limit: 63
            })
        );
        assert_eq!(
            DataFrameHeader::parse_within(&bytes[..31], 64).map(|_| ()),
            Err(FrameError::TooShort { given: 31 })
        );
    }

    #[test]
    fn every_error_names_the_docs_anchor() {
        extern crate std;
        use std::string::ToString;
        for error in [
            FrameError::TooShort { given: 1 },
            FrameError::Version { found: 2 },
            FrameError::Type { found: 3 },
            FrameError::Length { found: 4, limit: 5 },
        ] {
            let text = error.to_string();
            assert!(
                text.starts_with(&std::format!("frame rejected ({})", error.reason())),
                "{text}"
            );
            assert!(text.ends_with(DOC), "{text}");
        }
    }
}
