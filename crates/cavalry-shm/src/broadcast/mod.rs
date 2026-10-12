//! The broadcast buffer: `spec/broadcast-buffer.md`, version 0 (ADR 0014).
//!
//! The driver tells every client on a host what happened through one buffer: one
//! transmitter, the driver's conductor, and any number of receivers, the clients'
//! conductors, each at its own pace. The driver creates it with
//! [`BroadcastBuffer::create`] and writes through a [`Transmitter`], which never waits
//! for a receiver; a client opens it read-only with [`BroadcastBuffer::open`] and reads
//! through a [`Receiver`], which joins at the latest record, copies each record out and
//! is told `lapped` when the transmitter has overwritten what it had not read. Loss is
//! the contract, which is why the layout is control plane only: a record per event,
//! never per message.
//!
//! The module is in the layers of [`crate::counters`]: [`layout`] is the spec's bytes and
//! arithmetic on plain values, with no `unsafe`, so Miri and the vectors run against it;
//! `cells` puts the protocols behind atomics, which loom checks; `file` maps the file and
//! casts offsets to cells, and holds every `unsafe` block that touches a mapping. What
//! the broadcast buffer shares with the command ring, the header and the record word,
//! is one private module of the crate.
//!
//! # Example
//!
//! The driver creates the buffer, a client joins, the driver transmits an event and the
//! client receives it; the client stops reading, is lapped, and is told so.
//!
//! ```
//! use cavalry_shm::broadcast::{BroadcastBuffer, BroadcastError};
//!
//! fn main() -> Result<(), BroadcastError> {
//!     let dir = std::env::temp_dir().join(format!("cavalry-shm-broadcast-{}", std::process::id()));
//!     std::fs::create_dir_all(&dir)?;
//!
//!     // The driver creates the buffer once, 4 KiB here, and transmits every event on it.
//!     let buffer = BroadcastBuffer::create(&dir, 4096)?;
//!     let mut transmitter = buffer.transmitter()?;
//!
//!     // A client opens it read-only and joins at the latest record.
//!     let client = BroadcastBuffer::open(&dir, false)?;
//!     let mut receiver = client.receiver()?;
//!
//!     // An event of type 2 goes out; the client copies it out, validates the copy against
//!     // the tail intent, and is handed it.
//!     transmitter.transmit(2, b"publication ready")?;
//!     let received = receiver.receive(|type_id, event| {
//!         assert_eq!((type_id, event), (2, &b"publication ready"[..]));
//!     })?;
//!     assert!(received);
//!     assert!(!receiver.receive(|_, _| unreachable!())?, "nothing more yet");
//!
//!     // A client that does not keep up is lapped: the transmitter never waits for it,
//!     // and the client learns that it lost events and continues from the latest.
//!     for n in 0..9u8 {
//!         transmitter.transmit(3, &[n; 504])?;
//!     }
//!     let lapped = receiver.receive(|_, _| unreachable!()).unwrap_err();
//!     assert_eq!(lapped.reason(), "lapped");
//!     assert!(receiver.receive(|type_id, event| assert_eq!((type_id, event[0]), (3, 8)))?);
//!
//!     std::fs::remove_dir_all(&dir)?;
//!     Ok(())
//! }
//! ```

use core::fmt;

mod cells;
#[cfg(not(loom))]
mod file;
pub mod layout;

#[cfg(not(loom))]
pub use file::{BroadcastBuffer, Receiver, Transmitter};
pub use layout::{HeaderFields, Positions, RecordHeader, RecordState};

/// Why the buffer, a record or a request was refused. The first seven variants are the
/// spec's reader checks in the order they run; `Tail`, `Lapped` and `Record` are the
/// checks in use; `Message` and `Type` are the caller's errors that the spec says an API
/// refuses before anything is written. [`BroadcastError::reason`] is the spec's name;
/// the message says what to do and names the page that explains it.
#[derive(Debug)]
pub enum BroadcastError {
    /// The file is shorter than the trailer, or fewer than 128 header bytes were given.
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
    /// The capacity is not a power of two from [`layout::MIN_CAPACITY`] to
    /// [`layout::MAX_CAPACITY`].
    Capacity {
        /// The capacity found, or asked for.
        found: i64,
    },
    /// The file length is not capacity + 512.
    Length {
        /// The file's length.
        found: u64,
        /// The length the capacity needs.
        needed: u64,
    },
    /// The header read through the final mapping differs from the first read.
    Changed,
    /// The tail position, the tail intent or the latest record is outside the bound
    /// that holds for values loaded in that order. The writer is the driver, so the file
    /// is corrupt or not the driver's.
    Tail {
        /// The tail position found.
        tail: i64,
        /// The tail intent found.
        tail_intent: i64,
        /// The latest record found.
        latest: i64,
        /// The receiver's next record.
        next_record: i64,
    },
    /// The receiver's cursor was a whole ring behind the tail intent, before or after it
    /// read a record: records were lost. The receiver now sits at the latest record.
    Lapped {
        /// The cursor that was lapped.
        cursor: i64,
        /// The tail intent that lapped it.
        tail_intent: i64,
        /// The latest record, where the receiver continues.
        latest: i64,
    },
    /// A record's length or type is outside the bounds of the spec's "Record" table,
    /// and the receiver was not lapped: the driver wrote it, so the file is corrupt.
    Record {
        /// The record's index in the ring.
        index: u32,
        /// The length found.
        length: i32,
        /// The type found.
        type_id: i32,
    },
    /// A message longer than capacity ÷ 8, refused before anything is written.
    Message {
        /// The length asked for.
        found: usize,
        /// The longest this buffer allows.
        limit: usize,
    },
    /// A message type below 1, refused before anything is written.
    Type {
        /// The type asked for.
        found: i32,
    },
    /// The operating system refused to create, open, size or map the file.
    Io(std::io::Error),
}

const DOC_REJECTS: &str = "docs/reference/broadcast-buffer.md#what-a-reader-rejects";
const DOC_TRAILER: &str = "docs/reference/broadcast-buffer.md#trailer";
const DOC_RECORD: &str = "docs/reference/broadcast-buffer.md#record";
const DOC_LAPPED: &str = "docs/reference/broadcast-buffer.md#when-a-receiver-is-lapped";
const DOC_TRANSMIT: &str = "docs/reference/broadcast-buffer.md#transmitting";
const DOC_FILE: &str = "docs/reference/broadcast-buffer.md#file";

impl BroadcastError {
    /// The reason's name in `spec/broadcast-buffer.md`: `too-short`, `not-ready`,
    /// `magic`, `version`, `capacity`, `length`, `changed`, `tail`, `lapped`, `record`,
    /// `message`, `type` or `io`.
    pub const fn reason(&self) -> &'static str {
        match self {
            BroadcastError::TooShort { .. } => "too-short",
            BroadcastError::NotReady => "not-ready",
            BroadcastError::Magic { .. } => "magic",
            BroadcastError::Version { .. } => "version",
            BroadcastError::Capacity { .. } => "capacity",
            BroadcastError::Length { .. } => "length",
            BroadcastError::Changed => "changed",
            BroadcastError::Tail { .. } => "tail",
            BroadcastError::Lapped { .. } => "lapped",
            BroadcastError::Record { .. } => "record",
            BroadcastError::Message { .. } => "message",
            BroadcastError::Type { .. } => "type",
            BroadcastError::Io(_) => "io",
        }
    }
}

impl fmt::Display for BroadcastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = self.reason();
        match self {
            BroadcastError::TooShort { given } => write!(
                f,
                "broadcast buffer rejected ({reason}): the file is {given} bytes, a trailer is {}; \
                 the driver has not created it yet, or the directory is not a driver's; wait and \
                 read again, or check the directory; see {DOC_REJECTS}",
                layout::TRAILER_LENGTH
            ),
            BroadcastError::NotReady => write!(
                f,
                "broadcast buffer rejected ({reason}): the magic is zero, the creator has not \
                 finished; wait and read again; see {DOC_REJECTS}"
            ),
            BroadcastError::Magic { found } => write!(
                f,
                "broadcast buffer rejected ({reason}): the first eight bytes of the trailer are {}, \
                 not CVLYBCST; the file is not a broadcast buffer, check the directory; see \
                 {DOC_REJECTS}",
                Hex(found)
            ),
            BroadcastError::Version { found } => write!(
                f,
                "broadcast buffer rejected ({reason}): version {found}, this reader knows {}; the \
                 driver runs another Cavalry version, upgrade the older side; see {DOC_REJECTS}",
                layout::VERSION
            ),
            BroadcastError::Capacity { found } => write!(
                f,
                "broadcast buffer rejected ({reason}): capacity {found} is not a power of two from \
                 {} to {}; the file is corrupt or the capacity asked for is wrong, restart the \
                 driver; see {DOC_REJECTS}",
                layout::MIN_CAPACITY,
                layout::MAX_CAPACITY
            ),
            BroadcastError::Length { found, needed } => write!(
                f,
                "broadcast buffer rejected ({reason}): the file is {found} bytes, its capacity \
                 needs {needed}; the file is truncated or padded, restart the driver; see \
                 {DOC_REJECTS}"
            ),
            BroadcastError::Changed => write!(
                f,
                "broadcast buffer rejected ({reason}): the header changed between two reads; a \
                 driver is recreating the buffer, wait and open again; see {DOC_REJECTS}"
            ),
            BroadcastError::Tail {
                tail,
                tail_intent,
                latest,
                next_record,
            } => write!(
                f,
                "broadcast buffer rejected ({reason}): tail {tail}, tail intent {tail_intent} and \
                 latest record {latest} are not what the spec allows against a receiver at \
                 {next_record}; the driver wrote them, so the file is corrupt or not this driver's, \
                 close the client and restart the driver; see {DOC_TRAILER}"
            ),
            BroadcastError::Lapped {
                cursor,
                tail_intent,
                latest,
            } => write!(
                f,
                "broadcast receiver lapped ({reason}): the cursor {cursor} is a ring or more behind \
                 the tail intent {tail_intent}, events were lost, the receiver continues at \
                 {latest}; the client conductor is too slow or the buffer too small, close the \
                 client and register again; see {DOC_LAPPED}"
            ),
            BroadcastError::Record {
                index,
                length,
                type_id,
            } => write!(
                f,
                "broadcast buffer rejected ({reason}): the record at index {index} has length \
                 {length} and type {type_id}, outside the bounds the driver writes by; the file is \
                 corrupt or not this driver's, close the client and restart the driver; see \
                 {DOC_RECORD}"
            ),
            BroadcastError::Message { found, limit } => write!(
                f,
                "event refused ({reason}): a message of {found} bytes, this buffer allows {limit}; \
                 shorten the event, or create the buffer with a larger capacity; see {DOC_TRANSMIT}"
            ),
            BroadcastError::Type { found } => write!(
                f,
                "event refused ({reason}): message type {found} is below 1; −1 is padding, an \
                 event's type is 1 or more; see {DOC_RECORD}"
            ),
            BroadcastError::Io(error) => write!(
                f,
                "broadcast buffer ({reason}): {error}; check that the driver's directory exists and \
                 that this process may read, or create and write, the file inside it; see \
                 {DOC_FILE}"
            ),
        }
    }
}

impl std::error::Error for BroadcastError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BroadcastError::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for BroadcastError {
    fn from(error: std::io::Error) -> Self {
        BroadcastError::Io(error)
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
