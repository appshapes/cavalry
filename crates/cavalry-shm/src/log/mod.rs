//! The log buffer: `spec/log-buffer.md`, version 0 (ADR 0013).
//!
//! One publication's stream between processes on one host: three term buffers that
//! publishers fill with frames and subscribers read in place, and a metadata page that
//! says where the stream is. The driver creates a log with [`LogBuffers::create`] and
//! keeps it healthy on its duty cycle with the methods under "The driver" in the spec;
//! a publisher opens it with [`LogBuffers::open`] and writes through a [`TermAppender`],
//! whose [`TermAppender::try_claim`] is one fetch-and-add and one release store on the
//! way in and one release store on commit; a subscriber reads through a [`TermReader`],
//! which checks every frame as `spec/frame.md` says before it hands the body over. The
//! driver's replayer creates a replay's log at a position with [`LogBuffers::create_at`]
//! and writes recorded frames into it through a [`TermWriter`], by the claim protocol,
//! so that a subscriber reads a replay as it reads any stream (ADR 0030).
//!
//! The module is in the layers of [`crate::counters`]: [`layout`] is the spec's bytes
//! and arithmetic on plain values, with no `unsafe`, so Miri and the vectors run
//! against it; `cells` puts the publish protocols behind atomics, which loom checks;
//! `file` maps the file and casts offsets to cells, and holds every `unsafe` block that
//! touches a mapping.

use core::fmt;

use cavalry_proto::frame::FrameError;

mod cells;
#[cfg(not(loom))]
mod file;
pub mod layout;

#[cfg(not(loom))]
pub use file::{
    Block, BufferClaim, ClaimError, LogBuffers, Poll, TermAppender, TermReader, TermWriter,
    UnblockOutcome,
};
pub use layout::{ClaimRefused, HeaderFields, LogType, Positions, Tail};

/// Why a log, a position, a tail or a frame was refused. The first eleven variants are
/// the spec's reader checks in the order they run; `Position`, `Tail` and `Frame` are
/// the checks in use; `TooLong` is the caller's error that the spec says an API refuses
/// before anything is reserved. [`LogError::reason`] is the spec's name; the message
/// says what to do and names the page that explains it.
#[derive(Debug)]
pub enum LogError {
    /// The file is shorter than the metadata page, or fewer than 128 header bytes were
    /// given.
    TooShort {
        /// The length found.
        given: u64,
    },
    /// The magic is eight zero bytes: the creator has not finished. Wait and read again.
    NotReady,
    /// The magic is neither zero nor [`layout::MAGIC`].
    Magic {
        /// The eight bytes found.
        found: [u8; 8],
    },
    /// The version is not [`layout::VERSION`].
    Version {
        /// The version found.
        found: i32,
    },
    /// The type is not 0 or 1.
    Type {
        /// The type found.
        found: i32,
    },
    /// The term length is not a power of two from [`layout::MIN_TERM_LENGTH`] to
    /// [`layout::MAX_TERM_LENGTH`].
    TermLength {
        /// The term length found, or asked for.
        found: i64,
    },
    /// The MTU length is not a multiple of 32 from [`layout::MIN_MTU_LENGTH`] to
    /// [`layout::MAX_MTU_LENGTH`].
    Mtu {
        /// The MTU length found, or asked for.
        found: i64,
    },
    /// The publication window length is not 1 to term length ÷ 2.
    Window {
        /// The window found, or asked for.
        found: i64,
        /// The term length it was checked against.
        term_length: u32,
    },
    /// The default frame header is not what the spec says.
    FrameHeader,
    /// The file length is not 3 × term length + 4096.
    Length {
        /// The file's length.
        found: u64,
        /// The length the term length needs.
        needed: u64,
    },
    /// The header read through the final mapping differs from the first read.
    Changed,
    /// A position given to a reader is not a multiple of 32, is below the reader's
    /// position or is beyond the end of its term.
    Position {
        /// The position given.
        found: i64,
        /// The reader's position.
        current: i64,
        /// The end of the current term.
        limit: i64,
    },
    /// The active tail's term id is not initial term id + active term count.
    Tail {
        /// The term id in the tail.
        found: i32,
        /// The term id the active term count implies.
        expected: i32,
    },
    /// A frame in a term failed a check of `spec/frame.md`.
    Frame(FrameError),
    /// A recorded frame a replay writes carries a header field that is not its
    /// position's or its recording's: the term id, the term offset, the session id or
    /// the stream id (`spec/durable-log.md`, "Segment files"). Reported as `frame`.
    Mismatch {
        /// The field: `term id`, `term offset`, `session id` or `stream id`.
        field: &'static str,
        /// The value in the header.
        found: i64,
        /// The value the position or the recording says.
        expected: i64,
    },
    /// A message longer than the log allows: the maximum payload for a claim, the
    /// maximum message for an offer.
    TooLong {
        /// The length asked for.
        found: usize,
        /// The longest this log allows.
        limit: usize,
    },
    /// The operating system refused to create, open, size or map the file.
    Io(std::io::Error),
}

const DOC_REJECTS: &str = "docs/reference/log-buffer.md#what-a-reader-rejects";
const DOC_POSITIONS: &str = "docs/reference/log-buffer.md#positions";
const DOC_CLAIM: &str = "docs/reference/log-buffer.md#claim-commit-and-abort";
const DOC_FILE: &str = "docs/reference/log-buffer.md#file";

impl LogError {
    /// The reason's name in `spec/log-buffer.md`: `too-short`, `not-ready`, `magic`,
    /// `version`, `type`, `term-length`, `mtu`, `window`, `frame-header`, `length`,
    /// `changed`, `position`, `tail`, `frame`, `too-long` or `io`.
    pub const fn reason(&self) -> &'static str {
        match self {
            LogError::TooShort { .. } => "too-short",
            LogError::NotReady => "not-ready",
            LogError::Magic { .. } => "magic",
            LogError::Version { .. } => "version",
            LogError::Type { .. } => "type",
            LogError::TermLength { .. } => "term-length",
            LogError::Mtu { .. } => "mtu",
            LogError::Window { .. } => "window",
            LogError::FrameHeader => "frame-header",
            LogError::Length { .. } => "length",
            LogError::Changed => "changed",
            LogError::Position { .. } => "position",
            LogError::Tail { .. } => "tail",
            LogError::Frame(_) | LogError::Mismatch { .. } => "frame",
            LogError::TooLong { .. } => "too-long",
            LogError::Io(_) => "io",
        }
    }
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = self.reason();
        match self {
            LogError::TooShort { given } => write!(
                f,
                "log buffer rejected ({reason}): {given} bytes, a metadata page is {}; the driver has not \
                 created the log yet, or the file is not a log buffer; wait and read again, or check the \
                 directory; see {DOC_REJECTS}",
                layout::META_DATA_LENGTH
            ),
            LogError::NotReady => write!(
                f,
                "log buffer rejected ({reason}): the magic is zero, the creator has not finished; wait and \
                 read again; see {DOC_REJECTS}"
            ),
            LogError::Magic { found } => write!(
                f,
                "log buffer rejected ({reason}): the first eight bytes of the metadata page are {}, not \
                 CVLYLOGB; the file is not a log buffer, check the directory; see {DOC_REJECTS}",
                Hex(found)
            ),
            LogError::Version { found } => write!(
                f,
                "log buffer rejected ({reason}): version {found}, this reader knows {}; the driver runs \
                 another Cavalry version, upgrade the older side; see {DOC_REJECTS}",
                layout::VERSION
            ),
            LogError::Type { found } => write!(
                f,
                "log buffer rejected ({reason}): type {found} is not 0 (concurrent) or 1 (exclusive); the \
                 file is corrupt or from another Cavalry version, restart the driver; see {DOC_REJECTS}"
            ),
            LogError::TermLength { found } => write!(
                f,
                "log buffer rejected ({reason}): term length {found} is not a power of two from {} to {}; \
                 the file is corrupt, or the length asked for is wrong; see {DOC_REJECTS}",
                layout::MIN_TERM_LENGTH,
                layout::MAX_TERM_LENGTH
            ),
            LogError::Mtu { found } => write!(
                f,
                "log buffer rejected ({reason}): MTU length {found} is not a multiple of 32 from {} to {}; \
                 the file is corrupt, or the length asked for is wrong; see {DOC_REJECTS}",
                layout::MIN_MTU_LENGTH,
                layout::MAX_MTU_LENGTH
            ),
            LogError::Window { found, term_length } => write!(
                f,
                "log buffer rejected ({reason}): publication window length {found} is not 1 to {}; the \
                 file is corrupt, or the window asked for is wrong; see {DOC_REJECTS}",
                term_length / 2
            ),
            LogError::FrameHeader => write!(
                f,
                "log buffer rejected ({reason}): the default frame header is not a version 0 data frame \
                 header with frame length 0, flags C0, term offset 0, the initial term id and reserved \
                 value 0; the file is corrupt, restart the driver; see {DOC_REJECTS}"
            ),
            LogError::Length { found, needed } => write!(
                f,
                "log buffer rejected ({reason}): the file is {found} bytes, its term length needs {needed}; \
                 the file is truncated or padded, restart the driver; see {DOC_REJECTS}"
            ),
            LogError::Changed => write!(
                f,
                "log buffer rejected ({reason}): the header changed between two reads; a driver is \
                 recreating the log, wait and open again; see {DOC_REJECTS}"
            ),
            LogError::Position {
                found,
                current,
                limit,
            } => write!(
                f,
                "log buffer rejected ({reason}): position {found} is not a multiple of 32 between the \
                 reader's position {current} and the end of its term {limit}; the position came from \
                 another log or was computed wrongly; see {DOC_POSITIONS}"
            ),
            LogError::Tail { found, expected } => write!(
                f,
                "log buffer rejected ({reason}): the active tail holds term id {found}, the active term \
                 count says {expected}; a publisher wrote an impossible tail, close the publication; see \
                 {DOC_REJECTS}"
            ),
            LogError::Frame(error) => write!(
                f,
                "log buffer rejected ({reason}): a frame in the term failed a check, nothing after it is \
                 delivered: {error}"
            ),
            LogError::Mismatch {
                field,
                found,
                expected,
            } => write!(
                f,
                "log buffer rejected ({reason}): a recorded frame's {field} is {found}, its position \
                 and recording say {expected}; the segment file is damaged or is another recording's, \
                 and the replay ends at the frame before it: restore the segment file from a backup, \
                 and `cavalry doctor` names the recording; see \
                 docs/reference/durable-log.md#segment-files"
            ),
            LogError::TooLong { found, limit } => write!(
                f,
                "log buffer refused ({reason}): a message of {found} bytes, this log allows {limit}; \
                 split the message, or create the publication with a longer term; see {DOC_CLAIM}"
            ),
            LogError::Io(error) => write!(
                f,
                "log buffer ({reason}): {error}; check that the driver's directory exists and that this \
                 process may read, or create and write, publications/ inside it; see {DOC_FILE}"
            ),
        }
    }
}

impl std::error::Error for LogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LogError::Io(error) => Some(error),
            LogError::Frame(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LogError {
    fn from(error: std::io::Error) -> Self {
        LogError::Io(error)
    }
}

impl From<FrameError> for LogError {
    fn from(error: FrameError) -> Self {
        LogError::Frame(error)
    }
}

/// Eight bytes as sixteen lowercase hex digits, for the `magic` message.
struct Hex<'a>(&'a [u8; 8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
