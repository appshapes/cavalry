//! The bytes of `spec/counters-file.md`, version 0, on plain slices: the constants, the
//! header and record encodings, the offsets, and the reader checks in the spec's order.
//! No `unsafe` and no memory map here, so Miri runs every test that uses this module
//! and the spec's vectors run against it; `cells` and `file` put the same functions
//! over a mapping.

use zerocopy::little_endian::{I32, I64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::CountersError;

/// Length of the header in bytes.
pub const HEADER_LENGTH: usize = 128;
/// Length of one metadata record in bytes.
pub const RECORD_LENGTH: usize = 256;
/// Length of one value slot in bytes: the 128 bytes a core moves at once (ADR 0011).
pub const SLOT_LENGTH: usize = 128;
/// The first eight bytes of a counters file, `CVLYCNTR`, written last by the creator.
pub const MAGIC: [u8; 8] = *b"CVLYCNTR";
/// The version of `spec/counters-file.md` this crate reads and writes.
pub const VERSION: i32 = 0;
/// The smallest capacity a counters file may have.
pub const MIN_CAPACITY: u32 = 64;
/// The largest capacity a counters file may have.
pub const MAX_CAPACITY: u32 = 65_536;
/// Length of a record's key field in bytes.
pub const KEY_LENGTH: usize = 112;
/// The longest label a record holds, in bytes.
pub const MAX_LABEL_LENGTH: usize = 124;
/// The file's name inside the driver's directory.
pub const FILE_NAME: &str = "counters";

/// The header's table, as bytes. The magic is separate: it is the field the creator
/// writes last and a reader loads first, through an atomic, in `cells`.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawHeaderRest {
    version: I32,
    capacity: I32,
    creator_pid: I64,
    start_timestamp: I64,
    reserved: [u8; 96],
}

/// A metadata record's table, as bytes.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawRecord {
    state: I32,
    type_id: I32,
    reclaim_deadline: I64,
    key: [u8; KEY_LENGTH],
    label_length: I32,
    label: [u8; MAX_LABEL_LENGTH],
}

const _: () = assert!(core::mem::size_of::<RawHeaderRest>() == HEADER_LENGTH - 8);
const _: () = assert!(core::mem::size_of::<RawRecord>() == RECORD_LENGTH);

/// The header's fields after the magic, validated: the version is [`VERSION`] and the
/// capacity is a power of two from [`MIN_CAPACITY`] to [`MAX_CAPACITY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeaderFields {
    capacity: u32,
    creator_pid: i64,
    start_timestamp: i64,
}

impl HeaderFields {
    /// Fields for a file of `capacity` counters created by `creator_pid` at
    /// `start_timestamp` nanoseconds since the Unix epoch. Fails with
    /// [`CountersError::Capacity`] when the capacity is not a power of two from
    /// [`MIN_CAPACITY`] to [`MAX_CAPACITY`].
    pub fn new(
        capacity: u32,
        creator_pid: i64,
        start_timestamp: i64,
    ) -> Result<Self, CountersError> {
        if !(MIN_CAPACITY..=MAX_CAPACITY).contains(&capacity) || !capacity.is_power_of_two() {
            return Err(CountersError::Capacity {
                found: i64::from(capacity),
            });
        }
        Ok(Self {
            capacity,
            creator_pid,
            start_timestamp,
        })
    }

    /// The version this crate writes: [`VERSION`].
    pub const fn version(&self) -> i32 {
        VERSION
    }

    /// How many counters the file holds.
    pub const fn capacity(&self) -> u32 {
        self.capacity
    }

    /// The pid of the process that created the file.
    pub const fn creator_pid(&self) -> i64 {
        self.creator_pid
    }

    /// When the creator started, in nanoseconds since the Unix epoch.
    pub const fn start_timestamp(&self) -> i64 {
        self.start_timestamp
    }

    /// The file length these fields imply: 128 + capacity × 384.
    pub const fn file_length(&self) -> u64 {
        HEADER_LENGTH as u64 + self.capacity as u64 * (RECORD_LENGTH + SLOT_LENGTH) as u64
    }

    /// Offset of record `id`, or [`CountersError::Id`] when `id` is not below the capacity.
    pub fn record_offset(&self, id: u32) -> Result<usize, CountersError> {
        self.check_id(id)?;
        Ok(HEADER_LENGTH + id as usize * RECORD_LENGTH)
    }

    /// Offset of slot `id`, or [`CountersError::Id`] when `id` is not below the capacity.
    pub fn slot_offset(&self, id: u32) -> Result<usize, CountersError> {
        self.check_id(id)?;
        Ok(HEADER_LENGTH + self.capacity as usize * RECORD_LENGTH + id as usize * SLOT_LENGTH)
    }

    fn check_id(&self, id: u32) -> Result<(), CountersError> {
        if id < self.capacity {
            Ok(())
        } else {
            Err(CountersError::Id {
                found: id,
                capacity: self.capacity,
            })
        }
    }
}

/// The 128 bytes of a header for these fields, magic included.
pub fn encode_header(fields: &HeaderFields) -> [u8; HEADER_LENGTH] {
    let mut out = [0u8; HEADER_LENGTH];
    out[..8].copy_from_slice(&MAGIC);
    out[8..].copy_from_slice(&encode_header_rest(fields));
    out
}

/// The header's bytes after the magic.
pub(crate) fn encode_header_rest(fields: &HeaderFields) -> [u8; HEADER_LENGTH - 8] {
    let raw = RawHeaderRest {
        version: I32::new(VERSION),
        capacity: I32::new(fields.capacity as i32),
        creator_pid: I64::new(fields.creator_pid),
        start_timestamp: I64::new(fields.start_timestamp),
        reserved: [0; 96],
    };
    zerocopy::transmute!(raw)
}

/// Reads a header from the front of `bytes` for a file of `file_length` bytes and runs
/// the spec's checks in order: too short, not ready, magic, version, capacity, length.
/// The `changed` check needs two reads and lives in the file layer.
pub fn parse_header(bytes: &[u8], file_length: u64) -> Result<HeaderFields, CountersError> {
    if bytes.len() < HEADER_LENGTH {
        return Err(CountersError::TooShort {
            given: bytes.len() as u64,
        });
    }
    let mut magic = [0u8; 8];
    magic.copy_from_slice(&bytes[..8]);
    check_magic(magic)?;
    let mut rest = [0u8; HEADER_LENGTH - 8];
    rest.copy_from_slice(&bytes[8..HEADER_LENGTH]);
    parse_header_rest(&rest, file_length)
}

/// The not-ready and magic checks on the first eight bytes.
pub(crate) fn check_magic(magic: [u8; 8]) -> Result<(), CountersError> {
    if magic == [0; 8] {
        return Err(CountersError::NotReady);
    }
    if magic != MAGIC {
        return Err(CountersError::Magic { found: magic });
    }
    Ok(())
}

/// The version, capacity and length checks on the bytes after the magic.
pub(crate) fn parse_header_rest(
    rest: &[u8; HEADER_LENGTH - 8],
    file_length: u64,
) -> Result<HeaderFields, CountersError> {
    let Ok(raw) = RawHeaderRest::ref_from_bytes(rest) else {
        return Err(CountersError::TooShort {
            given: rest.len() as u64,
        });
    };
    let version = raw.version.get();
    if version != VERSION {
        return Err(CountersError::Version { found: version });
    }
    let capacity = raw.capacity.get();
    let fields = u32::try_from(capacity)
        .map_err(|_| CountersError::Capacity {
            found: i64::from(capacity),
        })
        .and_then(|c| HeaderFields::new(c, raw.creator_pid.get(), raw.start_timestamp.get()))?;
    let needed = fields.file_length();
    if file_length < needed {
        return Err(CountersError::Length {
            found: file_length,
            needed,
        });
    }
    Ok(fields)
}

/// A metadata record's state field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum State {
    /// Never allocated, or reset: not a counter.
    Unused = 0,
    /// A counter: the rest of the record and the slot are meaningful.
    Allocated = 1,
    /// Freed; reusable after the reclaim deadline. Not a counter.
    Reclaimed = -1,
}

impl TryFrom<i32> for State {
    type Error = CountersError;

    fn try_from(value: i32) -> Result<Self, CountersError> {
        match value {
            0 => Ok(State::Unused),
            1 => Ok(State::Allocated),
            -1 => Ok(State::Reclaimed),
            found => Err(CountersError::State { found }),
        }
    }
}

/// A metadata record's fields, validated: the state is one of [`State`]'s and the label
/// is at most [`MAX_LABEL_LENGTH`] bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RecordFields {
    state: State,
    type_id: i32,
    reclaim_deadline: i64,
    key: [u8; KEY_LENGTH],
    label_length: u8,
    label: [u8; MAX_LABEL_LENGTH],
}

impl RecordFields {
    /// A record with these fields. The key is padded with zero to [`KEY_LENGTH`] bytes
    /// and fails with [`CountersError::Key`] when longer; the label fails with
    /// [`CountersError::Label`] when longer than [`MAX_LABEL_LENGTH`] bytes.
    pub fn new(
        state: State,
        type_id: i32,
        reclaim_deadline: i64,
        key: &[u8],
        label: &str,
    ) -> Result<Self, CountersError> {
        if key.len() > KEY_LENGTH {
            return Err(CountersError::Key { found: key.len() });
        }
        let label_length = u8::try_from(label.len())
            .ok()
            .filter(|&n| usize::from(n) <= MAX_LABEL_LENGTH)
            .ok_or(CountersError::Label {
                found: i64::try_from(label.len()).unwrap_or(i64::MAX),
            })?;
        let mut key_bytes = [0u8; KEY_LENGTH];
        key_bytes[..key.len()].copy_from_slice(key);
        let mut label_bytes = [0u8; MAX_LABEL_LENGTH];
        label_bytes[..label.len()].copy_from_slice(label.as_bytes());
        Ok(Self {
            state,
            type_id,
            reclaim_deadline,
            key: key_bytes,
            label_length,
            label: label_bytes,
        })
    }

    /// The record's state.
    pub const fn state(&self) -> State {
        self.state
    }

    /// The `id` of the counter's entry in `counters.toml`.
    pub const fn type_id(&self) -> i32 {
        self.type_id
    }

    /// When a reclaimed record may be reused, in nanoseconds since the Unix epoch; 0
    /// when not reclaimed.
    pub const fn reclaim_deadline(&self) -> i64 {
        self.reclaim_deadline
    }

    /// The full key field, [`KEY_LENGTH`] bytes, zero-padded.
    pub const fn key(&self) -> &[u8; KEY_LENGTH] {
        &self.key
    }

    /// The label's bytes, without padding.
    pub fn label_bytes(&self) -> &[u8] {
        &self.label[..usize::from(self.label_length)]
    }

    /// The label, or an empty string when its bytes are not UTF-8.
    pub fn label(&self) -> &str {
        core::str::from_utf8(self.label_bytes()).unwrap_or("")
    }
}

/// The 256 bytes of a record for these fields.
pub fn encode_record(fields: &RecordFields) -> [u8; RECORD_LENGTH] {
    let raw = RawRecord {
        state: I32::new(fields.state as i32),
        type_id: I32::new(fields.type_id),
        reclaim_deadline: I64::new(fields.reclaim_deadline),
        key: fields.key,
        label_length: I32::new(i32::from(fields.label_length)),
        label: fields.label,
    };
    zerocopy::transmute!(raw)
}

/// Reads a record from the front of `bytes` and runs the spec's checks: too short,
/// state, label.
pub fn parse_record(bytes: &[u8]) -> Result<RecordFields, CountersError> {
    let Ok((raw, _)) = RawRecord::ref_from_prefix(bytes) else {
        return Err(CountersError::TooShort {
            given: bytes.len() as u64,
        });
    };
    let state = State::try_from(raw.state.get())?;
    let label_length = raw.label_length.get();
    let label_length = u8::try_from(label_length)
        .ok()
        .filter(|&n| usize::from(n) <= MAX_LABEL_LENGTH)
        .ok_or(CountersError::Label {
            found: i64::from(label_length),
        })?;
    Ok(RecordFields {
        state,
        type_id: raw.type_id.get(),
        reclaim_deadline: raw.reclaim_deadline.get(),
        key: raw.key,
        label_length,
        label: raw.label,
    })
}
