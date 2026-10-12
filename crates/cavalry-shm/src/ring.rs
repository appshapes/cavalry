//! What the two ring layouts share. `spec/command-ring.md` and `spec/broadcast-buffer.md`
//! have one shape: a power-of-two ring of records, then a trailer that begins with
//! Cavalry's 128-byte header and continues in 128-byte slots; the same record header,
//! a length and a type in one 8-byte word; and the same bounds on the capacity and the
//! message length (ADR 0014). The constants, the header encoding and its checks, and the
//! record word live here once. Each layout maps [`HeaderReject`] onto its own error, so
//! the reason a reader reports keeps the layout's name.

// Under loom, `file` is not compiled, so the parts only the file layers call are unused.
#![cfg_attr(loom, allow(dead_code))]

use zerocopy::little_endian::{I32, I64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::sync::{AtomicI64, AtomicU64, Ordering, UnsafeCell};

/// Length of the header at the start of a trailer, in bytes.
pub(crate) const HEADER_LENGTH: usize = 128;
/// The length of one trailer slot: the 128 bytes a core moves at once (ADR 0011).
pub(crate) const SLOT_LENGTH: usize = 128;
/// The smallest ring: one page, and a longest message of 512 bytes.
pub(crate) const MIN_CAPACITY: u32 = 4096;
/// The largest ring, 1 GiB: a capacity that fits an i32 with room for a trailer.
pub(crate) const MAX_CAPACITY: u32 = 1 << 30;
/// The length of a record's header: the length and the type, 4 bytes each.
pub(crate) const RECORD_HEADER_LENGTH: u32 = 8;
/// A record begins and ends at a multiple of this.
pub(crate) const RECORD_ALIGNMENT: u32 = 8;
/// The type of a padding record, which delivers nothing.
pub(crate) const PADDING_TYPE: i32 = -1;

/// The header's table after the magic, as bytes.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawHeaderRest {
    version: I32,
    capacity: I32,
    creator_pid: I64,
    start_timestamp: I64,
    reserved: [u8; 96],
}

const _: () = assert!(size_of::<RawHeaderRest>() == HEADER_LENGTH - 8);

/// The header's fields after the magic, validated: the capacity is a power of two from
/// [`MIN_CAPACITY`] to [`MAX_CAPACITY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RingHeader {
    pub(crate) capacity: u32,
    pub(crate) creator_pid: i64,
    pub(crate) start_timestamp: i64,
}

/// Why a ring's header was rejected: the seven checks both pages share, in the order
/// they run, minus `changed`, which needs two reads and lives in the file layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderReject {
    TooShort(u64),
    NotReady,
    Magic([u8; 8]),
    Version(i32),
    Capacity(i64),
    Length { found: u64, needed: u64 },
}

/// The `capacity` check: a power of two from [`MIN_CAPACITY`] to [`MAX_CAPACITY`].
pub(crate) fn check_capacity(capacity: i64) -> Result<u32, HeaderReject> {
    u32::try_from(capacity)
        .ok()
        .filter(|c| (MIN_CAPACITY..=MAX_CAPACITY).contains(c) && c.is_power_of_two())
        .ok_or(HeaderReject::Capacity(capacity))
}

/// The file length a ring of `capacity` with a trailer of `trailer_length` has.
pub(crate) const fn file_length(capacity: u32, trailer_length: usize) -> u64 {
    capacity as u64 + trailer_length as u64
}

/// The longest message a ring of `capacity` carries: capacity ÷ 8.
pub(crate) const fn max_message_length(capacity: u32) -> u32 {
    capacity / 8
}

/// A length rounded up to the record alignment.
pub(crate) const fn align_record(length: u32) -> u32 {
    (length + RECORD_ALIGNMENT - 1) & !(RECORD_ALIGNMENT - 1)
}

/// The header's bytes after the magic, for a layout at `version`.
pub(crate) fn encode_header_rest(version: i32, header: &RingHeader) -> [u8; HEADER_LENGTH - 8] {
    let raw = RawHeaderRest {
        version: I32::new(version),
        capacity: I32::new(header.capacity as i32),
        creator_pid: I64::new(header.creator_pid),
        start_timestamp: I64::new(header.start_timestamp),
        reserved: [0; 96],
    };
    zerocopy::transmute!(raw)
}

/// The 128 bytes of a header for these fields, `magic` included, for a layout at
/// `version`.
pub(crate) fn encode_header(
    magic: [u8; 8],
    version: i32,
    header: &RingHeader,
) -> [u8; HEADER_LENGTH] {
    let mut out = [0u8; HEADER_LENGTH];
    out[..8].copy_from_slice(&magic);
    out[8..].copy_from_slice(&encode_header_rest(version, header));
    out
}

/// The not-ready and magic checks on the first eight bytes.
pub(crate) fn check_magic(found: [u8; 8], expected: [u8; 8]) -> Result<(), HeaderReject> {
    if found == [0; 8] {
        return Err(HeaderReject::NotReady);
    }
    if found != expected {
        return Err(HeaderReject::Magic(found));
    }
    Ok(())
}

/// The version, capacity and length checks on the bytes after the magic, for a layout
/// at `expected_version` and a file of `file_length` bytes whose trailer is
/// `trailer_length` bytes.
pub(crate) fn parse_header_rest(
    rest: &[u8; HEADER_LENGTH - 8],
    file_length: u64,
    expected_version: i32,
    trailer_length: usize,
) -> Result<RingHeader, HeaderReject> {
    let Ok(raw) = RawHeaderRest::ref_from_bytes(rest) else {
        return Err(HeaderReject::TooShort(rest.len() as u64));
    };
    let version = raw.version.get();
    if version != expected_version {
        return Err(HeaderReject::Version(version));
    }
    let capacity = check_capacity(i64::from(raw.capacity.get()))?;
    let needed = self::file_length(capacity, trailer_length);
    if file_length != needed {
        return Err(HeaderReject::Length {
            found: file_length,
            needed,
        });
    }
    Ok(RingHeader {
        capacity,
        creator_pid: raw.creator_pid.get(),
        start_timestamp: raw.start_timestamp.get(),
    })
}

/// Reads a header from the front of `bytes` for a file of `file_length` bytes and runs
/// the shared checks in order: too short (a file shorter than the trailer, or fewer
/// than 128 header bytes), not ready, magic, version, capacity, length.
pub(crate) fn parse_header(
    bytes: &[u8],
    file_length: u64,
    magic: [u8; 8],
    version: i32,
    trailer_length: usize,
) -> Result<RingHeader, HeaderReject> {
    if file_length < trailer_length as u64 {
        return Err(HeaderReject::TooShort(file_length));
    }
    if bytes.len() < HEADER_LENGTH {
        return Err(HeaderReject::TooShort(bytes.len() as u64));
    }
    let mut found = [0u8; 8];
    found.copy_from_slice(&bytes[..8]);
    check_magic(found, magic)?;
    let mut rest = [0u8; HEADER_LENGTH - 8];
    rest.copy_from_slice(&bytes[8..HEADER_LENGTH]);
    parse_header_rest(&rest, file_length, version, trailer_length)
}

// ---- the record word ---------------------------------------------------------------

/// A record's header as the one 8-byte word both layouts store and load: the length in
/// the low 32 bits and the type in the high 32, each a little-endian i32, which on the
/// little-endian hosts this crate builds for is the native word.
pub(crate) const fn record_word(length: i32, type_id: i32) -> u64 {
    (length as u32 as u64) | ((type_id as u32 as u64) << 32)
}

/// The length in a record word.
pub(crate) const fn word_length(word: u64) -> i32 {
    word as u32 as i32
}

/// The type in a record word.
pub(crate) const fn word_type(word: u64) -> i32 {
    (word >> 32) as u32 as i32
}

/// The record word at `index` of a byte slice, little-endian, for the pure layout
/// functions and the vectors.
pub(crate) fn word_at(bytes: &[u8], index: u32) -> u64 {
    let start = index as usize;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[start..start + 8]);
    u64::from_le_bytes(raw)
}

// ---- the cells ----------------------------------------------------------------------

/// The 128-byte header at the start of a trailer: the magic as the atomic the creator
/// stores last and a reader loads first, then the other fields as bytes. The protocol
/// is the counters file's.
#[repr(C)]
pub(crate) struct RingHeaderCells {
    magic: AtomicU64,
    rest: UnsafeCell<[u8; HEADER_LENGTH - 8]>,
}

// SAFETY: `rest` is written once, by the creator, before its release store of `magic`
// (`publish`), and read only after an acquire load of `magic` found the layout's value
// (`read`), so no two parties touch the bytes at once; `magic` is an atomic (both
// pages, "Header").
unsafe impl Sync for RingHeaderCells {}

impl RingHeaderCells {
    /// Zero bytes: a header the creator has not published. Tests only; production
    /// headers are views over a mapping.
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            magic: AtomicU64::new(0),
            rest: UnsafeCell::new([0; HEADER_LENGTH - 8]),
        }
    }

    /// The creator's protocol: the fields after the magic, then `magic` with a release
    /// store. Everything else in the trailer was written before this call.
    pub(crate) fn publish(&self, magic: [u8; 8], version: i32, header: &RingHeader) {
        let rest = encode_header_rest(version, header);
        // SAFETY: the creator is the only writer of a header and publishes once, on a
        // file it created; no reader reads `rest` before the release store below, which
        // `read` acquires first.
        self.rest.with_mut(|p| unsafe { p.write(rest) });
        self.magic
            .store(u64::from_ne_bytes(magic), Ordering::Release);
    }

    /// A reader's protocol: the magic with an acquire load, then the rest, then the
    /// shared checks for a file of `file_length` bytes, in order.
    pub(crate) fn read(
        &self,
        magic: [u8; 8],
        version: i32,
        file_length: u64,
        trailer_length: usize,
    ) -> Result<RingHeader, HeaderReject> {
        let found = self.magic.load(Ordering::Acquire).to_ne_bytes();
        check_magic(found, magic)?;
        // SAFETY: the acquire load above saw the magic the creator stored with release
        // after writing `rest`, and the header never changes after that (both pages,
        // "Header"), so the bytes are complete and nobody writes them.
        let rest = self.rest.with(|p| unsafe { p.read() });
        parse_header_rest(&rest, file_length, version, trailer_length)
    }
}

/// One position in its 128-byte trailer slot (ADR 0011): the value, then bytes nobody
/// reads.
#[repr(C, align(128))]
pub(crate) struct PositionCells {
    value: AtomicI64,
    reserved: UnsafeCell<[u8; SLOT_LENGTH - 8]>,
}

// SAFETY: `value` is an atomic; `reserved` is written once by the creator, as zero, and
// read by nobody (both pages, "Trailer").
unsafe impl Sync for PositionCells {}

impl PositionCells {
    /// Zero. Tests only.
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            value: AtomicI64::new(0),
            reserved: UnsafeCell::new([0; SLOT_LENGTH - 8]),
        }
    }

    /// An acquire load: what a party that branches on another's position does.
    pub(crate) fn load_acquire(&self) -> i64 {
        self.value.load(Ordering::Acquire)
    }

    /// A plain load, for the one writer of a position reading its own.
    pub(crate) fn load_own(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
    }

    /// A release store: publishes what the writer did before it.
    pub(crate) fn store_release(&self, value: i64) {
        self.value.store(value, Ordering::Release);
    }

    /// A compare-and-swap, acquire and release both ways: the producers' tail.
    pub(crate) fn compare_exchange(&self, expected: i64, next: i64) -> Result<i64, i64> {
        self.value
            .compare_exchange(expected, next, Ordering::AcqRel, Ordering::Acquire)
    }

    /// A fetch-and-add: the correlation id counter. Relaxed: the value is an identity
    /// nobody orders memory by.
    pub(crate) fn fetch_add(&self, n: i64) -> i64 {
        self.value.fetch_add(n, Ordering::Relaxed)
    }
}

/// The ring's words: capacity ÷ 8 atomics, addressed by a byte index that is a
/// multiple of 8 and below the capacity. The file layer views the mapping as one; tests
/// and loom use a vector.
pub(crate) trait Words {
    /// The ring's capacity in bytes.
    fn capacity(&self) -> u32;
    /// The word at byte `index`.
    fn word(&self, index: u32) -> &AtomicU64;
}

/// A ring on plain memory: tests and loom.
#[cfg(any(test, loom))]
pub(crate) struct WordVec(pub(crate) Vec<AtomicU64>);

#[cfg(any(test, loom))]
impl WordVec {
    pub(crate) fn zeroed(capacity: u32) -> Self {
        Self((0..capacity / 8).map(|_| AtomicU64::new(0)).collect())
    }
}

#[cfg(any(test, loom))]
impl Words for WordVec {
    fn capacity(&self) -> u32 {
        self.0.len() as u32 * 8
    }

    fn word(&self, index: u32) -> &AtomicU64 {
        &self.0[index as usize / 8]
    }
}

#[cfg(not(loom))]
const _: () = {
    assert!(size_of::<RingHeaderCells>() == HEADER_LENGTH);
    assert!(align_of::<RingHeaderCells>() == 8);
    assert!(size_of::<PositionCells>() == SLOT_LENGTH);
    assert!(align_of::<PositionCells>() == SLOT_LENGTH);
};

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn a_record_word_packs_length_low_and_type_high() {
        let word = record_word(-24, 7);
        assert_eq!(word_length(word), -24);
        assert_eq!(word_type(word), 7);
        let bytes = word.to_le_bytes();
        assert_eq!(&bytes[..4], &(-24i32).to_le_bytes());
        assert_eq!(&bytes[4..], &7i32.to_le_bytes());
        assert_eq!(word_at(&bytes, 0), word);
        assert_eq!(word_type(record_word(8, PADDING_TYPE)), -1);
    }

    #[test]
    fn a_header_reads_not_ready_until_published() {
        let cells = RingHeaderCells::zeroed();
        let header = RingHeader {
            capacity: 4096,
            creator_pid: 1,
            start_timestamp: 2,
        };
        assert_eq!(
            cells.read(*b"CVLYCMDR", 0, 4096 + 768, 768),
            Err(HeaderReject::NotReady)
        );
        cells.publish(*b"CVLYCMDR", 0, &header);
        assert_eq!(cells.read(*b"CVLYCMDR", 0, 4096 + 768, 768), Ok(header));
        assert_eq!(
            cells.read(*b"CVLYCMDR", 1, 4096 + 768, 768),
            Err(HeaderReject::Version(0)),
            "a layout at another version refuses the header"
        );
        assert_eq!(
            cells.read(*b"CVLYBCST", 0, 4096 + 768, 768),
            Err(HeaderReject::Magic(*b"CVLYCMDR"))
        );
        assert!(matches!(
            cells.read(*b"CVLYCMDR", 0, 4096 + 767, 768),
            Err(HeaderReject::Length { .. })
        ));
    }
}

/// Nanoseconds since the Unix epoch, as every timestamp in `spec/`; 0 before the epoch.
pub(crate) fn now_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}
