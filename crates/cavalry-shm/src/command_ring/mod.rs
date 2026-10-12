//! The command ring: `spec/command-ring.md`, version 0 (ADR 0014).
//!
//! Every client on a host sends the driver its commands through one ring: many
//! producers, the clients, and one consumer, the driver's conductor. The driver creates
//! it with [`CommandRing::create`]; a client opens it with [`CommandRing::open`] and
//! writes through a [`Producer`], whose [`Producer::try_claim`] reserves a record with a
//! compare-and-swap on the tail and whose [`Claim::commit`] publishes it with one release
//! store; the driver reads through a [`Consumer`], which clears what it read and
//! unblocks a claim a dead client left behind. It is control plane: a record per
//! command, never per message.
//!
//! The module is in the layers of [`crate::counters`]: [`layout`] is the spec's bytes and
//! arithmetic on plain values, with no `unsafe`, so Miri and the vectors run against it;
//! `cells` puts the protocols behind atomics, which loom checks; `file` maps the file and
//! casts offsets to cells, and holds every `unsafe` block that touches a mapping. What
//! the command ring shares with the broadcast buffer, the header and the record word, is
//! one private module of the crate.
//!
//! # Example
//!
//! The driver creates the ring, a client takes a correlation id and claims a command,
//! the driver reads it, and a ring that is full says so.
//!
//! ```
//! use std::time::{Duration, SystemTime, UNIX_EPOCH};
//!
//! use cavalry_shm::command_ring::{CommandRing, CommandRingError};
//!
//! fn main() -> Result<(), CommandRingError> {
//!     let dir = std::env::temp_dir().join(format!("cavalry-shm-command-ring-{}", std::process::id()));
//!     std::fs::create_dir_all(&dir)?;
//!     let now = || SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as i64;
//!
//!     // The driver creates the ring once, 4 KiB here, and reads it on its duty cycle.
//!     let ring = CommandRing::create(&dir, 4096)?;
//!     let mut consumer = ring.consumer()?;
//!
//!     // A client opens it writable, takes a correlation id, and claims a record for a
//!     // command of type 1; it writes the command in place and commits with one store.
//!     let client = CommandRing::open(&dir, true)?;
//!     let mut producer = client.producer(Duration::from_secs(10))?;
//!     let correlation_id = producer.next_correlation_id();
//!     let mut claim = producer.try_claim(1, 8, now())?;
//!     claim.body().copy_from_slice(&correlation_id.to_le_bytes());
//!     claim.commit();
//!
//!     // The driver reads the command in place, then clears it and releases the head.
//!     let read = consumer.read(16, |type_id, message| {
//!         assert_eq!((type_id, message), (1, &correlation_id.to_le_bytes()[..]));
//!     })?;
//!     assert_eq!(read.messages, 1);
//!
//!     // A ring the driver does not read fills up: the claim that does not fit is `full`,
//!     // nothing is written, and the client decides whether to wait or give up.
//!     let full = loop {
//!         if let Err(error) = producer.write(1, &[0; 504], now()) {
//!             break error;
//!         }
//!     };
//!     assert_eq!(full.reason(), "full");
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
pub use file::{Claim, CommandRing, Consumer, Producer, Read, Rejection, UnblockOutcome};
pub use layout::{HeaderFields, RecordHeader, RecordState};

/// Why the ring, a record or a request was refused. The first seven variants are the
/// spec's reader checks in the order they run; `Tail`, `Record`, `Full`, `DriverClosed`
/// and `DriverTimeout` are the checks in use; `Message` and `Type` are the caller's
/// errors that the spec says an API refuses before anything is written.
/// [`CommandRingError::reason`] is the spec's name; the message says what to do and
/// names the page that explains it.
#[derive(Debug)]
pub enum CommandRingError {
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
    /// The file length is not capacity + 768.
    Length {
        /// The file's length.
        found: u64,
        /// The length the capacity needs.
        needed: u64,
    },
    /// The header read through the final mapping differs from the first read.
    Changed,
    /// A tail position is not a multiple of 8 or is below the head cache a producer
    /// loaded before it, or, as the consumer loads it after its own head, is below the
    /// head or more than capacity above it.
    Tail {
        /// The tail found.
        tail: i64,
        /// The head, or head cache, it was checked against.
        head: i64,
    },
    /// A record's length or type is outside the bounds of the spec's "Record" table. The
    /// ring is corrupt from that record on, and the consumer reads nothing more from it;
    /// a `Tail` an unblock finds does the same.
    Record {
        /// The record's index in the ring.
        index: u32,
        /// The length found.
        length: i32,
        /// The type found.
        type_id: i32,
    },
    /// A claim found fewer free bytes than it needs. Nothing was written.
    Full {
        /// The aligned length the claim needed.
        needed: u32,
    },
    /// A message longer than capacity ÷ 8, refused before anything is written.
    Message {
        /// The length asked for.
        found: usize,
        /// The longest this ring allows.
        limit: usize,
    },
    /// A message type below 1, refused before anything is written.
    Type {
        /// The type asked for.
        found: i32,
    },
    /// The consumer heartbeat is −1: the driver closed.
    DriverClosed,
    /// The consumer heartbeat is older than the driver timeout: the driver has stopped.
    DriverTimeout {
        /// How long ago the heartbeat was stored, in nanoseconds.
        age: i64,
        /// The driver timeout, in nanoseconds.
        timeout: i64,
    },
    /// The operating system refused to create, open, size or map the file.
    Io(std::io::Error),
}

const DOC_REJECTS: &str = "docs/reference/command-ring.md#what-a-reader-rejects";
const DOC_RECORD: &str = "docs/reference/command-ring.md#record";
const DOC_CLAIM: &str = "docs/reference/command-ring.md#claim-commit-and-abort";
const DOC_FULL: &str = "docs/reference/command-ring.md#when-the-ring-is-full";
const DOC_HEARTBEAT: &str = "docs/reference/command-ring.md#heartbeat-and-correlation-ids";
const DOC_FILE: &str = "docs/reference/command-ring.md#file";

impl CommandRingError {
    /// The reason's name in `spec/command-ring.md`: `too-short`, `not-ready`, `magic`,
    /// `version`, `capacity`, `length`, `changed`, `tail`, `record`, `full`, `message`,
    /// `type`, `driver-closed`, `driver-timeout` or `io`.
    pub const fn reason(&self) -> &'static str {
        match self {
            CommandRingError::TooShort { .. } => "too-short",
            CommandRingError::NotReady => "not-ready",
            CommandRingError::Magic { .. } => "magic",
            CommandRingError::Version { .. } => "version",
            CommandRingError::Capacity { .. } => "capacity",
            CommandRingError::Length { .. } => "length",
            CommandRingError::Changed => "changed",
            CommandRingError::Tail { .. } => "tail",
            CommandRingError::Record { .. } => "record",
            CommandRingError::Full { .. } => "full",
            CommandRingError::Message { .. } => "message",
            CommandRingError::Type { .. } => "type",
            CommandRingError::DriverClosed => "driver-closed",
            CommandRingError::DriverTimeout { .. } => "driver-timeout",
            CommandRingError::Io(_) => "io",
        }
    }
}

impl fmt::Display for CommandRingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = self.reason();
        match self {
            CommandRingError::TooShort { given } => write!(
                f,
                "command ring rejected ({reason}): the file is {given} bytes, a trailer is {}; the \
                 driver has not created it yet, or the directory is not a driver's; wait and read \
                 again, or check the directory; see {DOC_REJECTS}",
                layout::TRAILER_LENGTH
            ),
            CommandRingError::NotReady => write!(
                f,
                "command ring rejected ({reason}): the magic is zero, the creator has not finished; \
                 wait and read again; see {DOC_REJECTS}"
            ),
            CommandRingError::Magic { found } => write!(
                f,
                "command ring rejected ({reason}): the first eight bytes of the trailer are {}, not \
                 CVLYCMDR; the file is not a command ring, check the directory; see {DOC_REJECTS}",
                Hex(found)
            ),
            CommandRingError::Version { found } => write!(
                f,
                "command ring rejected ({reason}): version {found}, this reader knows {}; the driver \
                 runs another Cavalry version, upgrade the older side; see {DOC_REJECTS}",
                layout::VERSION
            ),
            CommandRingError::Capacity { found } => write!(
                f,
                "command ring rejected ({reason}): capacity {found} is not a power of two from {} to \
                 {}; the file is corrupt or the capacity asked for is wrong, restart the driver; see \
                 {DOC_REJECTS}",
                layout::MIN_CAPACITY,
                layout::MAX_CAPACITY
            ),
            CommandRingError::Length { found, needed } => write!(
                f,
                "command ring rejected ({reason}): the file is {found} bytes, its capacity needs \
                 {needed}; the file is truncated or padded, restart the driver; see {DOC_REJECTS}"
            ),
            CommandRingError::Changed => write!(
                f,
                "command ring rejected ({reason}): the header changed between two reads; a driver is \
                 recreating the ring, wait and open again; see {DOC_REJECTS}"
            ),
            CommandRingError::Tail { tail, head } => write!(
                f,
                "command ring rejected ({reason}): tail position {tail} against head {head} is not a \
                 multiple of 8 from the head to the head + capacity; a client wrote an impossible \
                 tail and the ring is corrupt; restart the driver, which recreates the ring, and if it \
                 happens again stop clients one at a time until it stops; see \
                 {DOC_REJECTS}"
            ),
            CommandRingError::Record {
                index,
                length,
                type_id,
            } => write!(
                f,
                "command ring rejected ({reason}): the record at index {index} has length {length} \
                 and type {type_id}, outside the bounds no client writes by the protocol; the ring is \
                 corrupt from here, every client's commands time out, restart the driver, which \
                 recreates the ring, and if it happens again stop clients one at a time until it \
                 stops; see {DOC_RECORD}"
            ),
            CommandRingError::Full { needed } => write!(
                f,
                "command refused ({reason}): the ring has fewer than {needed} free bytes; the driver \
                 is slow or stopped, check its consumer heartbeat, and retry after it has read; see \
                 {DOC_FULL}"
            ),
            CommandRingError::Message { found, limit } => write!(
                f,
                "command refused ({reason}): a message of {found} bytes, this ring allows {limit}; \
                 shorten the command, or create the ring with a larger capacity; see {DOC_CLAIM}"
            ),
            CommandRingError::Type { found } => write!(
                f,
                "command refused ({reason}): message type {found} is below 1; −1 is padding and 0 \
                 is unclaimed, a command's type is 1 or more; see {DOC_RECORD}"
            ),
            CommandRingError::DriverClosed => write!(
                f,
                "command refused ({reason}): the consumer heartbeat is −1, the driver closed; start \
                 a driver and open the ring again; see {DOC_HEARTBEAT}"
            ),
            CommandRingError::DriverTimeout { age, timeout } => write!(
                f,
                "command refused ({reason}): the consumer heartbeat is {age} ns old, the driver \
                 timeout is {timeout} ns; the driver has stopped, restart it; see {DOC_HEARTBEAT}"
            ),
            CommandRingError::Io(error) => write!(
                f,
                "command ring ({reason}): {error}; check that the driver's directory exists and that \
                 this process may read, or create and write, the file inside it; see {DOC_FILE}"
            ),
        }
    }
}

impl std::error::Error for CommandRingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CommandRingError::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CommandRingError {
    fn from(error: std::io::Error) -> Self {
        CommandRingError::Io(error)
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
