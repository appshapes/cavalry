//! The header, the record and the slot as cells: the atomics the publish protocols of
//! `spec/counters-file.md` need, around the plain bytes of [`layout`]. A cell is never
//! constructed in production: `file` casts an offset of the mapping to one, so each
//! struct here is `#[repr(C)]` at the spec's offsets and holds nothing but atomics and
//! byte arrays, valid for every bit pattern. Every plain field sits in an `UnsafeCell`
//! and is read only after the acquire load its protocol names, so a reference to a
//! cell that another process is writing is sound. Under `cfg(loom)` the atomics are
//! loom's and the tests at the bottom check the protocols; `just loom` runs them.

// Under loom, `file` is not compiled (loom's atomics have another size), so the parts of
// the cells only `file` calls are unused there.
#![cfg_attr(loom, allow(dead_code))]

use super::CountersError;
use super::layout::{
    self, HEADER_LENGTH, HeaderFields, MAGIC, RECORD_LENGTH, RecordFields, SLOT_LENGTH, State,
};
use crate::sync::{AtomicI32, AtomicI64, AtomicU64, Ordering, UnsafeCell};

// The file's integers are little-endian (spec/README.md) and its atomics are the host's,
// so the byte image is the spec's on a little-endian host only. Cavalry's targets are
// x86-64 and aarch64. A big-endian port would swap every value here and in `layout`,
// which is a layout version by another name.
#[cfg(target_endian = "big")]
compile_error!(
    "cavalry-shm writes the little-endian layout of spec/counters-file.md through host-order atomics; big-endian hosts are not supported"
);

/// Where a record's plain tail begins: the key, the label length and the label.
const TAIL_OFFSET: usize = 16;
/// The length of that tail: 112 + 4 + 124.
const TAIL_LENGTH: usize = RECORD_LENGTH - TAIL_OFFSET;

/// The 128-byte header: the magic as the atomic the creator stores last and a reader
/// loads first, then the other fields as bytes.
#[repr(C)]
pub(crate) struct HeaderCells {
    magic: AtomicU64,
    rest: UnsafeCell<[u8; HEADER_LENGTH - 8]>,
}

// SAFETY: `rest` is written once, by the creator, before its release store of `magic`
// (`publish`), and read only after an acquire load of `magic` found the spec's value
// (`read`), so no two parties touch the bytes at once; `magic` is an atomic
// (spec, "Header").
unsafe impl Sync for HeaderCells {}

impl HeaderCells {
    /// Zero bytes: a header the creator has not published. Tests only; production
    /// headers are views over a mapping.
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            magic: AtomicU64::new(0),
            rest: UnsafeCell::new([0; HEADER_LENGTH - 8]),
        }
    }

    /// The creator's protocol: the fields after the magic, then the magic with a
    /// release store.
    pub(crate) fn publish(&self, fields: &HeaderFields) {
        let rest = layout::encode_header_rest(fields);
        // SAFETY: the creator is the only writer of a header and publishes once, on a
        // file it created (`file::CountersFile::create`); no reader reads `rest` before
        // the release store below, which `read` acquires first.
        self.rest.with_mut(|p| unsafe { p.write(rest) });
        self.magic
            .store(u64::from_ne_bytes(MAGIC), Ordering::Release);
    }

    /// A reader's protocol: the magic with an acquire load, then the rest, then the
    /// spec's checks for a file of `file_length` bytes, in order.
    pub(crate) fn read(&self, file_length: u64) -> Result<HeaderFields, CountersError> {
        let magic = self.magic.load(Ordering::Acquire).to_ne_bytes();
        layout::check_magic(magic)?;
        // SAFETY: the acquire load above saw the magic the creator stored with release
        // after writing `rest`, and the header never changes after that (spec,
        // "Header"), so the bytes are complete and nobody writes them.
        let rest = self.rest.with(|p| unsafe { p.read() });
        layout::parse_header_rest(&rest, file_length)
    }
}

/// A 256-byte metadata record: the state as the atomic the driver stores last and a
/// reader loads first, the reclaim deadline as the atomic only the driver reads, and
/// the type id, key, label length and label as bytes.
#[repr(C)]
pub(crate) struct RecordCells {
    state: AtomicI32,
    type_id: UnsafeCell<[u8; 4]>,
    reclaim_deadline: AtomicI64,
    tail: UnsafeCell<[u8; TAIL_LENGTH]>,
}

// SAFETY: the plain bytes are written by the driver, the sole writer, before its
// release store of `state` to 1 (`publish`), and read only after an acquire load of
// `state` found 1 (`read`); `state` and `reclaim_deadline` are atomics (spec,
// "Metadata record"). The reuse case is argued at `publish`.
unsafe impl Sync for RecordCells {}

impl RecordCells {
    /// Zero bytes: an unused record. Tests only.
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            state: AtomicI32::new(0),
            type_id: UnsafeCell::new([0; 4]),
            reclaim_deadline: AtomicI64::new(0),
            tail: UnsafeCell::new([0; TAIL_LENGTH]),
        }
    }

    /// The driver's protocol: type id, key, label length and label, then the reclaim
    /// deadline, then the state with a release store. The caller has set the slot to 0
    /// first (spec, "Metadata record").
    pub(crate) fn publish(&self, fields: &RecordFields) {
        let bytes = layout::encode_record(fields);
        let mut type_id = [0u8; 4];
        type_id.copy_from_slice(&bytes[4..8]);
        let mut tail = [0u8; TAIL_LENGTH];
        tail.copy_from_slice(&bytes[TAIL_OFFSET..]);
        // SAFETY: the driver is the sole writer (one `file::CountersFile` per file, and
        // `allocate` takes `&mut self`), and it publishes only a record whose state is 0,
        // or -1 with its reclaim deadline passed (`file::CountersFile::allocate`). A
        // reader reads these bytes only after an acquire load of `state` found 1
        // (`read`), which the release store below is the first to make true again. A
        // reader that acquired 1 before the record was freed and is still reading is
        // what the deadline is for: the spec gives it until the deadline to finish, the
        // same bound as Aeron's free-for-reuse deadline. That bound is in time, which
        // loom cannot model; the loom test below covers the first allocation.
        self.type_id.with_mut(|p| unsafe { p.write(type_id) });
        self.tail.with_mut(|p| unsafe { p.write(tail) });
        self.reclaim_deadline
            .store(fields.reclaim_deadline(), Ordering::Relaxed);
        self.state.store(fields.state() as i32, Ordering::Release);
    }

    /// The driver's protocol for freeing: the deadline, then the state -1 with a release
    /// store. The record may be published again after `reusable_after`.
    pub(crate) fn free(&self, reusable_after: i64) {
        self.reclaim_deadline
            .store(reusable_after, Ordering::Relaxed);
        self.state.store(State::Reclaimed as i32, Ordering::Release);
    }

    /// The state, with an acquire load, or [`CountersError::State`] for a value the spec
    /// does not name.
    pub(crate) fn state(&self) -> Result<State, CountersError> {
        State::try_from(self.state.load(Ordering::Acquire))
    }

    /// When a reclaimed record may be reused, in nanoseconds since the Unix epoch. Only
    /// the driver reads it (spec, "Metadata record").
    pub(crate) fn reclaim_deadline(&self) -> i64 {
        self.reclaim_deadline.load(Ordering::Relaxed)
    }

    /// A reader's protocol: the state with an acquire load, then the rest only when the
    /// state is 1. `Ok(None)` for a record that is not a counter.
    pub(crate) fn read(&self) -> Result<Option<RecordFields>, CountersError> {
        let state = self.state.load(Ordering::Acquire);
        if State::try_from(state)? != State::Allocated {
            return Ok(None);
        }
        // SAFETY: the acquire load above found 1, which the driver stores with release
        // only after writing these bytes (`publish`), and the driver rewrites them only
        // after storing -1 and waiting out the reclaim deadline, within which this read
        // completes (the time bound argued at `publish`).
        let type_id = self.type_id.with(|p| unsafe { p.read() });
        // SAFETY: as above.
        let tail = self.tail.with(|p| unsafe { p.read() });
        let mut bytes = [0u8; RECORD_LENGTH];
        bytes[..4].copy_from_slice(&state.to_le_bytes());
        bytes[4..8].copy_from_slice(&type_id);
        bytes[8..16].copy_from_slice(&self.reclaim_deadline.load(Ordering::Relaxed).to_le_bytes());
        bytes[TAIL_OFFSET..].copy_from_slice(&tail);
        layout::parse_record(&bytes).map(Some)
    }
}

/// A 128-byte value slot: the value, then bytes nobody reads. The alignment is the
/// slot's own, so two counters never share the 128 bytes a core moves at once
/// (ADR 0011, applied by ADR 0012).
#[repr(C, align(128))]
pub(crate) struct SlotCells {
    value: AtomicI64,
    reserved: UnsafeCell<[u8; SLOT_LENGTH - 8]>,
}

// SAFETY: `value` is an atomic; `reserved` is written by the driver, the sole writer,
// only while the slot's record is not allocated, and read by nobody (spec, "Value slot").
unsafe impl Sync for SlotCells {}

impl SlotCells {
    /// Zero bytes. Tests only.
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            value: AtomicI64::new(0),
            reserved: UnsafeCell::new([0; SLOT_LENGTH - 8]),
        }
    }

    /// Allocation: the value 0 and the reserved bytes 0, before the record is published.
    pub(crate) fn reset(&self) {
        // SAFETY: nobody reads `reserved` (spec, "Value slot": written as 0 at
        // allocation, never read), and the driver, the sole writer, resets a slot only
        // while its record's state is not 1, so no reader reads the slot at all.
        self.reserved
            .with_mut(|p| unsafe { p.write([0; SLOT_LENGTH - 8]) });
        self.value.store(0, Ordering::Relaxed);
    }

    /// Adds `n`, relaxed: the one operation on the data plane.
    pub(crate) fn add(&self, n: i64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Stores `value`, relaxed: a gauge moving to a level.
    pub(crate) fn set(&self, value: i64) {
        self.value.store(value, Ordering::Relaxed);
    }

    /// Loads the value, relaxed. Nobody branches on it on the data plane.
    pub(crate) fn get(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
    }
}

// The spec's offsets and sizes, checked at compile time. Under loom the atomics are
// loom's and have another size; the layout is then not under test, the protocols are.
#[cfg(not(loom))]
const _: () = {
    assert!(size_of::<HeaderCells>() == HEADER_LENGTH);
    assert!(align_of::<HeaderCells>() == 8);
    assert!(core::mem::offset_of!(HeaderCells, rest) == 8);
    assert!(size_of::<RecordCells>() == RECORD_LENGTH);
    assert!(align_of::<RecordCells>() == 8);
    assert!(core::mem::offset_of!(RecordCells, type_id) == 4);
    assert!(core::mem::offset_of!(RecordCells, reclaim_deadline) == 8);
    assert!(core::mem::offset_of!(RecordCells, tail) == TAIL_OFFSET);
    assert!(size_of::<SlotCells>() == SLOT_LENGTH);
    assert!(align_of::<SlotCells>() == SLOT_LENGTH);
};

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn header() -> HeaderFields {
        HeaderFields::new(64, 4242, 1_760_000_000_000_000_000).unwrap()
    }

    fn record() -> RecordFields {
        RecordFields::new(
            State::Allocated,
            7,
            0,
            &[1, 0, 0, 0, 0xe9, 3, 0, 0],
            "publication.back_pressure 1 1001",
        )
        .unwrap()
    }

    #[test]
    fn a_header_reads_not_ready_until_published_then_its_fields() {
        let cells = HeaderCells::zeroed();
        let length = header().file_length();
        assert!(matches!(cells.read(length), Err(CountersError::NotReady)));
        cells.publish(&header());
        assert_eq!(cells.read(length).unwrap(), header());
        assert!(matches!(
            cells.read(length - 1),
            Err(CountersError::Length { .. })
        ));
    }

    #[test]
    fn a_record_reads_none_until_published_then_its_fields_then_none_when_freed() {
        let cells = RecordCells::zeroed();
        assert_eq!(cells.state().unwrap(), State::Unused);
        assert!(cells.read().unwrap().is_none());
        cells.publish(&record());
        assert_eq!(cells.state().unwrap(), State::Allocated);
        assert_eq!(cells.read().unwrap(), Some(record()));
        cells.free(5);
        assert_eq!(cells.state().unwrap(), State::Reclaimed);
        assert_eq!(cells.reclaim_deadline(), 5);
        assert!(cells.read().unwrap().is_none());
    }

    #[test]
    fn a_slot_adds_sets_and_resets() {
        let cells = SlotCells::zeroed();
        cells.add(3);
        cells.add(-1);
        assert_eq!(cells.get(), 2);
        cells.set(10);
        assert_eq!(cells.get(), 10);
        cells.reset();
        assert_eq!(cells.get(), 0);
    }
}

#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::thread;

    use super::*;

    fn header() -> HeaderFields {
        HeaderFields::new(64, 4242, 1_760_000_000_000_000_000).unwrap()
    }

    fn record() -> RecordFields {
        RecordFields::new(
            State::Allocated,
            7,
            0,
            &[1, 0, 0, 0, 0xe9, 3, 0, 0],
            "publication.back_pressure 1 1001",
        )
        .unwrap()
    }

    /// The creator writes the fields and releases the magic; a reader that acquires the
    /// magic sees every field, and never reads the fields before that. loom explores
    /// every interleaving of the two threads and fails on a read that races the write.
    #[test]
    fn a_reader_acquires_the_header_the_creator_released() {
        loom::model(|| {
            let cells = Arc::new(HeaderCells::zeroed());
            let creator = {
                let cells = Arc::clone(&cells);
                thread::spawn(move || cells.publish(&header()))
            };
            loop {
                match cells.read(header().file_length()) {
                    Err(CountersError::NotReady) => thread::yield_now(),
                    Ok(read) => {
                        assert_eq!(read, header());
                        break;
                    }
                    Err(other) => panic!("{other}"),
                }
            }
            creator.join().unwrap();
        });
    }

    /// The driver writes the record's bytes and releases the state; a reader that
    /// acquires state 1 sees every byte, and reads nothing while the state is 0.
    #[test]
    fn a_reader_acquires_the_record_the_driver_released() {
        loom::model(|| {
            let cells = Arc::new(RecordCells::zeroed());
            let driver = {
                let cells = Arc::clone(&cells);
                thread::spawn(move || cells.publish(&record()))
            };
            loop {
                match cells.read() {
                    Ok(None) => thread::yield_now(),
                    Ok(Some(read)) => {
                        assert_eq!(read, record());
                        break;
                    }
                    Err(other) => panic!("{other}"),
                }
            }
            driver.join().unwrap();
        });
    }
}
