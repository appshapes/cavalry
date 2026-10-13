//! The counters file: `spec/counters-file.md`, version 0 (ADR 0012).
//!
//! The driver creates the file once with [`CountersFile::create`], which allocates
//! every static counter in `counters.toml`, and hands out a [`Counter`] per allocated
//! record; a counter's [`Counter::increment`] is one relaxed add in a slot nobody else
//! writes. Everyone else opens the file read-only with [`CountersReader::open`], which
//! runs the spec's checks in the spec's order and reports the first that fails as a
//! [`CountersError`] whose [`CountersError::reason`] is the spec's name.
//!
//! The module is in layers. [`layout`] is the spec's bytes on plain slices, with no
//! `unsafe`, so Miri and the spec's vectors run against it. `cells` puts the same
//! encodings behind the atomics the publish protocols need, and loom checks those
//! protocols. `file` maps the file and casts offsets to cells; it holds every `unsafe`
//! block that touches a mapping. [`generated`] is written from `counters.toml` by
//! `scripts/gen-counters.py`.
//!
//! # Example
//!
//! The driver creates the file and allocates a counter; a reader opens the file and reads
//! the counter back; the file fills up and says so.
//!
//! ```
//! use cavalry_shm::counters::generated::{COUNTERS, COUNTERS_ALLOCATED, COUNTERS_ALLOCATION_FAILED};
//! use cavalry_shm::counters::{CounterId, CountersError, CountersFile, CountersReader, Scope};
//!
//! fn main() -> Result<(), CountersError> {
//!     let dir = std::env::temp_dir().join(format!("cavalry-shm-counters-{}", std::process::id()));
//!     std::fs::create_dir_all(&dir)?;
//!
//!     // The driver creates the file once, with room for 64 counters. Every static counter
//!     // in counters.toml is allocated with it, in the record whose index is its id.
//!     let mut driver = CountersFile::create(&dir, 64)?;
//!     let statics = COUNTERS.iter().filter(|def| def.scope == Scope::Static).count() as i64;
//!
//!     // An instance counter: one per publication, image or whatever it counts. The type id
//!     // is the counters.toml id of the entry a later feature adds; the key and the label
//!     // say which instance. Moving it is one relaxed add, the data plane's only operation.
//!     let gaps = driver.allocate(CounterId(7), &1001i32.to_le_bytes(), "image.gaps 1001")?;
//!     gaps.increment();
//!
//!     // Everyone else opens the file read-only. A reader checks the header before it trusts
//!     // anything, and a record's state before its value.
//!     let reader = CountersReader::open(&dir)?;
//!     assert_eq!(reader.read(COUNTERS_ALLOCATED.record())?, statics + 1);
//!     assert_eq!(reader.read(gaps.id())?, 1);
//!
//!     // The capacity is fixed at creation. When every record is in use, an allocation fails
//!     // with `full`, counters.allocation_failed moves, and whatever needed it is refused.
//!     for n in (statics + 1)..64 {
//!         driver.allocate(CounterId(7), &[], &format!("image.gaps {n}"))?;
//!     }
//!     let full = driver.allocate(CounterId(7), &[], "image.gaps 64").unwrap_err();
//!     assert_eq!(full.reason(), "full");
//!     assert_eq!(reader.read(COUNTERS_ALLOCATION_FAILED.record())?, 1);
//!
//!     std::fs::remove_dir_all(&dir)?;
//!     Ok(())
//! }
//! ```

use core::fmt;

mod cells;
#[cfg(not(loom))]
mod file;
pub mod generated;
pub mod layout;

#[cfg(not(loom))]
pub use file::{Counter, CounterValue, CountersFile, CountersReader};

/// The `id` of an entry in `counters.toml`: the type id of every record allocated for
/// that entry, 0 or more. A static counter also occupies the record at this index
/// (`spec/counters-file.md`, "Metadata record"). The constants in [`generated`] are the
/// only values the driver passes; `just lint-counters` keeps them in step with the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CounterId(pub i32);

impl CounterId {
    /// The record and slot a static counter of this id occupies: the id as an index. An
    /// instance counter's record is wherever [`CountersFile::allocate`] put it, which
    /// [`Counter::id`] says. A negative id, which `just lint-counters` refuses, becomes
    /// an index no file has, so every use of it is [`CountersError::Id`].
    pub const fn record(self) -> u32 {
        self.0 as u32
    }
}

/// What a counter's value does over time: `counters.toml`'s `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Never decreases while allocated.
    Counter,
    /// Moves both ways.
    Gauge,
}

/// How many counters an entry stands for: `counters.toml`'s `scope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// One counter, allocated when the file is created, in the record whose index is
    /// the entry's id.
    Static,
    /// One counter per key, allocated when the instance is created; the entry's `doc`
    /// says the key's layout.
    Instance,
}

/// One entry of `counters.toml`, as [`generated::COUNTERS`] lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CounterDef {
    /// The entry's `id`, stable and never reused.
    pub id: CounterId,
    /// The entry's `name`: lowercase and dotted, the layout or component first. The
    /// label of a static counter, and the prefix of an instance counter's label.
    pub name: &'static str,
    /// The entry's `kind`.
    pub kind: Kind,
    /// The entry's `scope`.
    pub scope: Scope,
    /// One sentence: what a change in the value means.
    pub doc: &'static str,
    /// What an operator does when this counter says something is wrong, or empty.
    pub doctor: &'static str,
}

/// Why the file, a record or a request was refused. The first seven variants are the
/// spec's reader checks in the order they run, then its three record checks, then the
/// writer's refusals. [`CountersError::reason`] is the name the spec and the vectors
/// use; the message says what to do and names the page that explains it.
#[derive(Debug)]
pub enum CountersError {
    /// The file is shorter than [`layout::HEADER_LENGTH`] bytes.
    TooShort {
        /// The file's length, or the bytes given.
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
    /// The file is shorter than its capacity needs.
    Length {
        /// The file's length.
        found: u64,
        /// The length the capacity needs: 128 + capacity × 384.
        needed: u64,
    },
    /// The header read through the final mapping differs from the first read.
    Changed,
    /// A counter id is not below the capacity.
    Id {
        /// The id given.
        found: u32,
        /// The file's capacity; ids run from 0 to one below it.
        capacity: u32,
    },
    /// A record's state is not −1, 0 or 1.
    State {
        /// The state found.
        found: i32,
    },
    /// A record's label length is not 0 to [`layout::MAX_LABEL_LENGTH`].
    Label {
        /// The label length found, or asked for.
        found: i64,
    },
    /// A key longer than [`layout::KEY_LENGTH`] bytes was given to an allocation.
    Key {
        /// The key's length.
        found: usize,
    },
    /// No record is unused or reclaimed past its deadline: the file is full.
    Full {
        /// The file's capacity, every record of which is in use.
        capacity: u32,
    },
    /// The record holds no counter: it was freed, or never allocated.
    Unallocated {
        /// The record's id.
        id: u32,
    },
    /// The operating system refused to create, open, size or map the file.
    Io(std::io::Error),
}

/// The docs anchor the reader's rejections name.
const DOC_REJECTS: &str = "docs/reference/counters-file.md#what-a-reader-rejects";
/// The docs anchor the record's bounds name.
const DOC_RECORD: &str = "docs/reference/counters-file.md#metadata-record";
/// The docs anchor a full file names.
const DOC_FULL: &str = "docs/reference/counters-file.md#when-the-file-is-full";
/// The docs anchor an operating system error names.
const DOC_FILE: &str = "docs/reference/counters-file.md#file";

impl CountersError {
    /// The reason's name in `spec/counters-file.md`: `too-short`, `not-ready`, `magic`,
    /// `version`, `capacity`, `length`, `changed`, `id`, `state`, `label`, `key`,
    /// `full`, `unallocated` or `io`.
    pub const fn reason(&self) -> &'static str {
        match self {
            CountersError::TooShort { .. } => "too-short",
            CountersError::NotReady => "not-ready",
            CountersError::Magic { .. } => "magic",
            CountersError::Version { .. } => "version",
            CountersError::Capacity { .. } => "capacity",
            CountersError::Length { .. } => "length",
            CountersError::Changed => "changed",
            CountersError::Id { .. } => "id",
            CountersError::State { .. } => "state",
            CountersError::Label { .. } => "label",
            CountersError::Key { .. } => "key",
            CountersError::Full { .. } => "full",
            CountersError::Unallocated { .. } => "unallocated",
            CountersError::Io(_) => "io",
        }
    }
}

impl fmt::Display for CountersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = self.reason();
        match self {
            CountersError::TooShort { given } => write!(
                f,
                "counters file rejected ({reason}): the file is {given} bytes, a header is \
                 {}; the driver has not created it yet, or the directory is not a driver's; \
                 wait and read again, or check the directory; see {DOC_REJECTS}",
                layout::HEADER_LENGTH
            ),
            CountersError::NotReady => write!(
                f,
                "counters file rejected ({reason}): the magic is zero, the creator has not \
                 finished; wait and read again; see {DOC_REJECTS}"
            ),
            CountersError::Magic { found } => write!(
                f,
                "counters file rejected ({reason}): the first eight bytes are {}, not \
                 CVLYCNTR; the file is not a counters file, check the directory; see \
                 {DOC_REJECTS}",
                Hex(found)
            ),
            CountersError::Version { found } => write!(
                f,
                "counters file rejected ({reason}): version {found}, this reader knows {}; \
                 the driver runs another Cavalry version, upgrade the older side; see \
                 {DOC_REJECTS}",
                layout::VERSION
            ),
            CountersError::Capacity { found } => write!(
                f,
                "counters file rejected ({reason}): capacity {found} is not a power of two \
                 from {} to {}; the file is corrupt or from another Cavalry version, restart \
                 the driver; see {DOC_REJECTS}",
                layout::MIN_CAPACITY,
                layout::MAX_CAPACITY
            ),
            CountersError::Length { found, needed } => write!(
                f,
                "counters file rejected ({reason}): the file is {found} bytes, its capacity \
                 needs {needed}; the file is truncated, restart the driver; see {DOC_REJECTS}"
            ),
            CountersError::Changed => write!(
                f,
                "counters file rejected ({reason}): the header changed between two reads; a \
                 driver is recreating the file, wait and open again; see {DOC_REJECTS}"
            ),
            CountersError::Id { found, capacity } => write!(
                f,
                "counter rejected ({reason}): counter {found} is not below the capacity \
                 {capacity}; the id belongs to another file or another driver; see \
                 {DOC_REJECTS}"
            ),
            CountersError::State { found } => write!(
                f,
                "counter rejected ({reason}): record state {found} is not -1, 0 or 1; the \
                 file is corrupt, restart the driver; see {DOC_REJECTS}"
            ),
            CountersError::Label { found } => write!(
                f,
                "counter rejected ({reason}): label length {found} is not 0 to {}; the file \
                 is corrupt, or the label asked for is too long; see {DOC_REJECTS}",
                layout::MAX_LABEL_LENGTH
            ),
            CountersError::Key { found } => write!(
                f,
                "counter refused ({reason}): a key of {found} bytes does not fit the \
                 {}-byte key field; the counter's doc in counters.toml defines a shorter \
                 key; see {DOC_RECORD}",
                layout::KEY_LENGTH
            ),
            CountersError::Full { capacity } => write!(
                f,
                "counter refused ({reason}): every one of {capacity} records is in use; \
                 restart the driver with a larger counters file capacity, new publications \
                 and subscriptions are refused until then; see {DOC_FULL}"
            ),
            CountersError::Unallocated { id } => write!(
                f,
                "counter refused ({reason}): record {id} holds no counter, it was freed or \
                 never allocated; read counters.allocated for how many exist; see \
                 {DOC_RECORD}"
            ),
            CountersError::Io(error) => write!(
                f,
                "counters file ({reason}): {error}; check that the driver's directory exists \
                 and that this process may read and write it; see {DOC_FILE}"
            ),
        }
    }
}

impl std::error::Error for CountersError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CountersError::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CountersError {
    fn from(error: std::io::Error) -> Self {
        CountersError::Io(error)
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
