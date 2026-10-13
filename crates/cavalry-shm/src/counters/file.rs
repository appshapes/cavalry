//! The counters file on disk: creating it, mapping it, and casting offsets of the
//! mapping to the cells of `cells`. Every `unsafe` block that touches a mapping is in
//! this module; `cells` and `layout` never see a file.

use core::fmt;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use memmap2::{Mmap, MmapMut, MmapOptions};

use super::cells::{HeaderCells, RecordCells, SlotCells};
use super::generated::{COUNTERS, COUNTERS_ALLOCATED, COUNTERS_ALLOCATION_FAILED};
use super::layout::{
    FILE_NAME, HEADER_LENGTH, HeaderFields, MAX_CAPACITY, RECORD_LENGTH, RecordFields, SLOT_LENGTH,
    State,
};
use super::{CounterId, CountersError, Scope};

// The largest file, 128 + 65536 × 384 bytes, fits a usize on every target, so a file
// length converts with `as`.
const _: () = assert!(
    (HEADER_LENGTH as u64 + MAX_CAPACITY as u64 * (RECORD_LENGTH + SLOT_LENGTH) as u64)
        <= usize::MAX as u64
);

/// The writable mapping of the creator or the read-only mapping of a reader.
enum Map {
    Writer(MmapMut),
    Reader(Mmap),
}

/// A mapping with the header fields it was checked against. Every cell is a view at an
/// offset of `base`, and every `unsafe` block below relies on the same three facts: the
/// mapping is at least `fields.file_length()` bytes, checked in `new`, and the offsets
/// come from `fields`, which checks each id against the capacity; the base is
/// page-aligned, so an offset that is a multiple of 128 keeps every alignment the cells
/// ask for; and the cells are `#[repr(C)]` atomics and byte arrays, valid for every bit
/// pattern, which other processes write only through the protocols `cells` names.
struct Mapped {
    /// Keeps the mapping alive; `base` points into it. A reader's map is read-only, so a
    /// reader cannot write even by mistake.
    _map: Map,
    base: *const u8,
    length: usize,
    fields: HeaderFields,
}

// SAFETY: `base` points into `_map`, which lives as long as `self`, and the only accesses
// through it are the atomics and the publish protocols of `cells`, whose types are
// `Sync`; the pointer itself is never written after `new`.
unsafe impl Send for Mapped {}
// SAFETY: as above.
unsafe impl Sync for Mapped {}

impl Mapped {
    /// A mapping of at least `fields.file_length()` bytes, or [`CountersError::Length`].
    fn new(mut map: Map, fields: HeaderFields) -> Result<Self, CountersError> {
        // The writer's pointer comes from `&mut [u8]`, so it keeps permission to write; the
        // map is never dereferenced again, so nothing invalidates it.
        let (base, length) = match &mut map {
            Map::Writer(map) => (map.as_mut_ptr().cast_const(), map.len()),
            Map::Reader(map) => (map.as_ptr(), map.len()),
        };
        let needed = fields.file_length();
        if (length as u64) < needed {
            return Err(CountersError::Length {
                found: length as u64,
                needed,
            });
        }
        Ok(Self {
            _map: map,
            base,
            length,
            fields,
        })
    }

    fn header_cells(&self) -> &HeaderCells {
        // SAFETY: the mapping is at least `file_length()` bytes, which is at least
        // `HEADER_LENGTH` (`new`), so the header is inside it; `base` is page-aligned,
        // more than the 8 bytes `HeaderCells` needs; the cell is atomics and bytes; the
        // reference lives no longer than `self`, which owns the map.
        unsafe { &*self.base.cast::<HeaderCells>() }
    }

    fn record(&self, id: u32) -> Result<&RecordCells, CountersError> {
        let offset = self.fields.record_offset(id)?;
        debug_assert!(offset + RECORD_LENGTH <= self.length);
        // SAFETY: `record_offset` checked `id` against the capacity, so the record ends
        // at or before `file_length()`, which the mapping covers (`new`); the offset is
        // 128 + id × 256 from a page-aligned base, a multiple of the 8 bytes
        // `RecordCells` needs; the cell is atomics and bytes; the reference lives no
        // longer than `self`.
        Ok(unsafe { &*self.base.add(offset).cast::<RecordCells>() })
    }

    fn slot(&self, id: u32) -> Result<&SlotCells, CountersError> {
        let offset = self.fields.slot_offset(id)?;
        Ok(self.slot_at(offset))
    }

    /// The slot at `offset`, a value `fields.slot_offset` returned for this mapping
    /// (`slot`, `Counter::new`, or `CountersReader::value`, which `CounterValue` keeps).
    fn slot_at(&self, offset: usize) -> &SlotCells {
        debug_assert!(offset + SLOT_LENGTH <= self.length && offset % SLOT_LENGTH == 0);
        // SAFETY: `offset` came from `slot_offset` on this mapping's fields (`slot`;
        // `Counter::new`, which keeps one; and `CountersReader::value`, which
        // `CounterValue` keeps), so the slot ends at or before `file_length()`,
        // which the mapping covers (`new`); the offset is 128 + capacity × 256 + id × 128
        // from a page-aligned base, a multiple of the 128 bytes `SlotCells` is aligned
        // to; the cell is atomics and bytes; the reference lives no longer than `self`.
        unsafe { &*self.base.add(offset).cast::<SlotCells>() }
    }
}

/// Nanoseconds since the Unix epoch, as every timestamp in `spec/`; 0 before the epoch.
fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

/// The record a static counter occupies: its id. [`CountersError::Full`] when the file
/// has no such record, because `counters.toml` has a static counter beyond this
/// capacity.
fn static_record(id: CounterId, capacity: u32) -> Result<u32, CountersError> {
    let record = id.record();
    if record < capacity {
        Ok(record)
    } else {
        Err(CountersError::Full { capacity })
    }
}

/// The driver's handle on the counters file it created: the one writer. It allocates
/// and frees counters and hands out a [`Counter`] per allocated record. Dropping it
/// unmaps the file and leaves it on disk for readers; the next driver to start finds it
/// and [`CountersFile::create`] refuses to overwrite it, so removing a file a crashed
/// driver left is the driver's job before it creates a new one.
pub struct CountersFile {
    mapped: Arc<Mapped>,
    allocated: Counter,
    allocation_failed: Counter,
}

impl CountersFile {
    /// Creates `dir/counters` for `capacity` counters and publishes it: sizes the file,
    /// allocates every static counter of `counters.toml` in the record whose index is
    /// its id, sets `counters.allocated` to their number, and writes the magic last
    /// (`spec/counters-file.md`, "Header"). The capacity is a power of two from
    /// [`super::layout::MIN_CAPACITY`] to [`super::layout::MAX_CAPACITY`], or the error is
    /// [`CountersError::Capacity`]; a file already at that path is
    /// [`CountersError::Io`]; a static counter beyond the capacity is
    /// [`CountersError::Full`].
    pub fn create(dir: &Path, capacity: u32) -> Result<Self, CountersError> {
        let fields = HeaderFields::new(capacity, i64::from(std::process::id()), now_nanos())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.join(FILE_NAME))?;
        let length = fields.file_length();
        file.set_len(length)?;
        // SAFETY: memmap2 marks mapping a file `unsafe` because another process could
        // truncate the file under the mapping and turn a load into a fault. This call
        // created the file, in the driver's directory, which is the trust boundary every
        // shared layout assumes: ADR 0010 bounds the values a peer writes, not whether
        // the file exists. A truncation is a fault, never silent misbehaviour.
        let map = unsafe { MmapOptions::new().len(length as usize).map_mut(&file)? };
        let mapped = Arc::new(Mapped::new(Map::Writer(map), fields)?);
        let mut count: i64 = 0;
        for def in COUNTERS.iter().filter(|def| def.scope == Scope::Static) {
            let record = static_record(def.id, capacity)?;
            let fields = RecordFields::new(State::Allocated, def.id.0, 0, &[], def.name)?;
            mapped.slot(record)?.reset();
            mapped.record(record)?.publish(&fields);
            count += 1;
        }
        let allocated = Counter::new(
            Arc::clone(&mapped),
            static_record(COUNTERS_ALLOCATED, capacity)?,
        )?;
        let allocation_failed = Counter::new(
            Arc::clone(&mapped),
            static_record(COUNTERS_ALLOCATION_FAILED, capacity)?,
        )?;
        allocated.set(count);
        mapped.header_cells().publish(&fields);
        Ok(Self {
            mapped,
            allocated,
            allocation_failed,
        })
    }

    /// The header's fields: capacity, creator pid and start timestamp.
    pub fn header(&self) -> HeaderFields {
        self.mapped.fields
    }

    /// How many counters the file holds.
    pub fn capacity(&self) -> u32 {
        self.mapped.fields.capacity()
    }

    /// Allocates a counter of type `type_id` with `key` and `label` in the first record
    /// that is unused or reclaimed past its deadline, sets its value to 0, publishes
    /// the record, and adds one to `counters.allocated`. When no record is free the
    /// error is [`CountersError::Full`] and `counters.allocation_failed` gains one
    /// (`spec/counters-file.md`, "When the file is full"). A key over
    /// [`super::layout::KEY_LENGTH`] bytes is [`CountersError::Key`] and a label over
    /// [`super::layout::MAX_LABEL_LENGTH`] bytes is [`CountersError::Label`]; neither touches
    /// the file.
    pub fn allocate(
        &mut self,
        type_id: CounterId,
        key: &[u8],
        label: &str,
    ) -> Result<Counter, CountersError> {
        let fields = RecordFields::new(State::Allocated, type_id.0, 0, key, label)?;
        let capacity = self.capacity();
        let now = now_nanos();
        let mut free = None;
        for id in 0..capacity {
            let record = self.mapped.record(id)?;
            let reusable = match record.state()? {
                State::Unused => true,
                State::Reclaimed => record.reclaim_deadline() <= now,
                State::Allocated => false,
            };
            if reusable {
                free = Some(id);
                break;
            }
        }
        let Some(id) = free else {
            self.allocation_failed.increment();
            return Err(CountersError::Full { capacity });
        };
        self.mapped.slot(id)?.reset();
        self.mapped.record(id)?.publish(&fields);
        self.allocated.increment();
        Counter::new(Arc::clone(&self.mapped), id)
    }

    /// Frees counter `id`: its record becomes reclaimed and may be allocated again after
    /// `reusable_after` nanoseconds since the Unix epoch, and `counters.allocated` loses
    /// one. A handle on the counter keeps working on its slot; readers see no counter.
    /// [`CountersError::Id`] when `id` is not below the capacity, and
    /// [`CountersError::Unallocated`] when the record holds no counter.
    pub fn free(&mut self, id: u32, reusable_after: i64) -> Result<(), CountersError> {
        let record = self.mapped.record(id)?;
        if record.state()? != State::Allocated {
            return Err(CountersError::Unallocated { id });
        }
        record.free(reusable_after);
        self.allocated.add(-1);
        Ok(())
    }

    /// A handle on the allocated counter in record `id`, static counters included: a
    /// static counter's record is its id. [`CountersError::Id`] when `id` is not below
    /// the capacity, and [`CountersError::Unallocated`] when the record holds no counter.
    pub fn counter(&self, id: u32) -> Result<Counter, CountersError> {
        if self.mapped.record(id)?.state()? != State::Allocated {
            return Err(CountersError::Unallocated { id });
        }
        Counter::new(Arc::clone(&self.mapped), id)
    }
}

impl fmt::Debug for CountersFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CountersFile")
            .field("header", &self.mapped.fields)
            .finish()
    }
}

/// One allocated counter, owned by the one thread that moves it. [`Counter::increment`]
/// and [`Counter::add`] are one relaxed add each, with no allocation, lock, syscall or
/// panic path: the data plane's one operation on this file. The handle is `Clone`,
/// `Send` and `Sync`, borrows nothing from the [`CountersFile`], and keeps the mapping
/// alive on its own.
#[derive(Clone)]
pub struct Counter {
    mapped: Arc<Mapped>,
    offset: usize,
    id: u32,
}

impl Counter {
    fn new(mapped: Arc<Mapped>, id: u32) -> Result<Self, CountersError> {
        let offset = mapped.fields.slot_offset(id)?;
        Ok(Self { mapped, offset, id })
    }

    /// The counter's id: the index of its record and its slot.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Adds one, relaxed.
    pub fn increment(&self) {
        self.mapped.slot_at(self.offset).add(1);
    }

    /// Adds `n`, relaxed. A gauge may pass a negative `n`; a counter never decreases
    /// while allocated (`counters.toml`, `kind`).
    pub fn add(&self, n: i64) {
        self.mapped.slot_at(self.offset).add(n);
    }

    /// Stores `value`, relaxed: a gauge moving to a level.
    pub fn set(&self, value: i64) {
        self.mapped.slot_at(self.offset).set(value);
    }

    /// The value, relaxed. Readers use [`CountersReader::read`]; this is for the owner.
    pub fn get(&self) -> i64 {
        self.mapped.slot_at(self.offset).get()
    }

    /// Stores a position with a release store, so that everything this thread read
    /// before, the frames it consumed, happens before a driver's acquire load of the
    /// position (`spec/log-buffer.md`, "Reading"; ADR 0013). For a counter that is a
    /// position; other counters use [`Counter::set`].
    pub fn store_release(&self, position: i64) {
        self.mapped.slot_at(self.offset).set_release(position);
    }

    /// Loads a position with an acquire load: what the driver does before it branches
    /// on a subscriber's position to clean a term (`spec/log-buffer.md`, "The driver").
    pub fn load_acquire(&self) -> i64 {
        self.mapped.slot_at(self.offset).get_acquire()
    }
}

impl fmt::Debug for Counter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Counter").field("id", &self.id).finish()
    }
}

/// A client's writable view of a counters file: the one mapping through which it stores
/// the values of the counters the driver allocated for it, its `subscription.position`
/// (`spec/counters-file.md`, "Value slot": a value is written by its owner; ADR 0013),
/// and reads any counter. It never allocates or frees a record: the header and the
/// metadata records are the driver's alone, and [`CountersFile`] is the driver's type.
/// Writing a counter that is not this client's is a protocol violation the driver's
/// bounds checks catch (ADR 0010); nothing here can stop it, as nothing in Aeron can.
pub struct CountersWriter {
    mapped: Arc<Mapped>,
}

impl CountersWriter {
    /// Opens `dir/counters` writable and runs the checks of [`CountersReader::open`].
    pub fn open(dir: &Path) -> Result<Self, CountersError> {
        Ok(Self {
            mapped: open_mapped(dir, true)?,
        })
    }

    /// The header's fields: capacity, creator pid and start timestamp.
    pub fn header(&self) -> HeaderFields {
        self.mapped.fields
    }

    /// How many counters the file holds; ids run from 0 to one below it.
    pub fn capacity(&self) -> u32 {
        self.mapped.fields.capacity()
    }

    /// The owner's handle on counter `id`, for the counter the driver allocated for this
    /// client and named in its event: its `subscription.position`, stored with
    /// [`Counter::store_release`] after each poll. [`CountersError::Id`] when `id` is not
    /// below the capacity, and [`CountersError::Unallocated`] when the record holds no
    /// counter.
    pub fn counter(&self, id: u32) -> Result<Counter, CountersError> {
        if self.mapped.record(id)?.state()? != State::Allocated {
            return Err(CountersError::Unallocated { id });
        }
        Counter::new(Arc::clone(&self.mapped), id)
    }

    /// A reader's handle on counter `id`'s value, as [`CountersReader::value`].
    pub fn value(&self, id: u32) -> Result<CounterValue, CountersError> {
        if self.mapped.record(id)?.state()? != State::Allocated {
            return Err(CountersError::Unallocated { id });
        }
        let offset = self.mapped.fields.slot_offset(id)?;
        Ok(CounterValue {
            mapped: Arc::clone(&self.mapped),
            offset,
            id,
        })
    }

    /// The value of counter `id`, as [`CountersReader::read`].
    pub fn read(&self, id: u32) -> Result<i64, CountersError> {
        if self.mapped.record(id)?.state()? != State::Allocated {
            return Err(CountersError::Unallocated { id });
        }
        Ok(self.mapped.slot(id)?.get())
    }
}

impl fmt::Debug for CountersWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CountersWriter")
            .field("capacity", &self.capacity())
            .finish()
    }
}

/// A reader's handle on one counter's value, from [`CountersReader::value`]: the
/// read-only side of [`Counter`]. One load per call, nothing checked, nothing allocated;
/// `Clone`, `Send` and `Sync`, and it keeps the mapping alive on its own.
#[derive(Clone)]
pub struct CounterValue {
    mapped: Arc<Mapped>,
    offset: usize,
    id: u32,
}

impl CounterValue {
    /// The counter's id.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The value, relaxed: a counter nobody branches on.
    pub fn get(&self) -> i64 {
        self.mapped.slot_at(self.offset).get()
    }

    /// The value with an acquire load: a limit or a position, whose owner stored it with
    /// release after writing what the value publishes (`spec/log-buffer.md`).
    pub fn load_acquire(&self) -> i64 {
        self.mapped.slot_at(self.offset).get_acquire()
    }
}

impl fmt::Debug for CounterValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CounterValue")
            .field("id", &self.id)
            .finish()
    }
}

/// Opens `dir/counters`, read-only or writable, with the spec's checks in the spec's
/// order (`spec/counters-file.md`, "What a reader rejects"): the file's size from `fstat`
/// before any mapping (`too-short`), the header through a first mapping of 128 bytes
/// (`not-ready`, `magic`, `version`, `capacity`, `length`), then the header again through
/// the final mapping, which must equal the first read (`changed`). What
/// [`CountersReader::open`] and [`CountersWriter::open`] share.
fn open_mapped(dir: &Path, writable: bool) -> Result<Arc<Mapped>, CountersError> {
    let file = OpenOptions::new()
        .read(true)
        .write(writable)
        .open(dir.join(FILE_NAME))?;
    let file_length = file.metadata()?.len();
    if file_length < HEADER_LENGTH as u64 {
        return Err(CountersError::TooShort { given: file_length });
    }
    // SAFETY: as at `CountersFile::create`: memmap2 marks the mapping `unsafe` because
    // another process could truncate the file underneath it, and the driver's directory
    // is the trust boundary; a truncation faults, never lies.
    let probe = unsafe { MmapOptions::new().len(HEADER_LENGTH).map(&file)? };
    let first = {
        // SAFETY: the probe maps `HEADER_LENGTH` bytes at a page-aligned base, which the
        // file's size covers (checked above); `HeaderCells` is atomics and bytes; the
        // reference lives no longer than `probe`.
        let header = unsafe { &*probe.as_ptr().cast::<HeaderCells>() };
        header.read(file_length)?
    };
    drop(probe);
    let length = first.file_length() as usize;
    let map = if writable {
        // SAFETY: as above; writable, so that the owner of a counter the driver
        // allocated for it can store the value, which the spec gives the owner alone
        // ("Value slot"; ADR 0013). ADR 0010 bounds what a peer writes; the driver's
        // directory is the trust boundary, as for every mapping here.
        Map::Writer(unsafe { MmapOptions::new().len(length).map_mut(&file)? })
    } else {
        // SAFETY: as above.
        Map::Reader(unsafe { MmapOptions::new().len(length).map(&file)? })
    };
    let mapped = Mapped::new(map, first)?;
    let second = mapped.header_cells().read(file_length)?;
    if second != first {
        return Err(CountersError::Changed);
    }
    Ok(Arc::new(mapped))
}

/// A read-only view of a counters file: what clients, `cavalry doctor` and the control
/// plane hold. It never writes the file; its mapping is read-only.
pub struct CountersReader {
    mapped: Arc<Mapped>,
}

impl CountersReader {
    /// Opens `dir/counters` read-only and runs the spec's checks in the spec's order
    /// (`spec/counters-file.md`, "What a reader rejects"): the file's size from `fstat`
    /// before any mapping (`too-short`), the header through a first mapping of 128
    /// bytes (`not-ready`, `magic`, `version`, `capacity`, `length`), then the header
    /// again through the final mapping, which must equal the first read (`changed`).
    /// The error's [`CountersError::reason`] names the check that failed. A reader
    /// that gets [`CountersError::NotReady`] waits and opens again.
    pub fn open(dir: &Path) -> Result<Self, CountersError> {
        Ok(Self {
            mapped: open_mapped(dir, false)?,
        })
    }

    /// The header's fields: capacity, creator pid and start timestamp.
    pub fn header(&self) -> HeaderFields {
        self.mapped.fields
    }

    /// How many counters the file holds; ids run from 0 to one below it.
    pub fn capacity(&self) -> u32 {
        self.mapped.fields.capacity()
    }

    /// The metadata of counter `id`, or `None` when the record holds no counter.
    /// [`CountersError::Id`] when `id` is not below the capacity, and
    /// [`CountersError::State`] or [`CountersError::Label`] when the record's bytes are
    /// outside the spec's bounds.
    pub fn record(&self, id: u32) -> Result<Option<RecordFields>, CountersError> {
        self.mapped.record(id)?.read()
    }

    /// The value of counter `id`, after an acquire load of its record's state.
    /// [`CountersError::Id`] when `id` is not below the capacity, and
    /// [`CountersError::Unallocated`] when the record holds no counter.
    pub fn read(&self, id: u32) -> Result<i64, CountersError> {
        if self.mapped.record(id)?.state()? != State::Allocated {
            return Err(CountersError::Unallocated { id });
        }
        Ok(self.mapped.slot(id)?.get())
    }

    /// [`CountersReader::read`] with an acquire load of the value: for a position, whose
    /// owner stored it with release after reading what it consumed
    /// (`spec/log-buffer.md`, "The driver"; ADR 0013).
    pub fn read_acquire(&self, id: u32) -> Result<i64, CountersError> {
        if self.mapped.record(id)?.state()? != State::Allocated {
            return Err(CountersError::Unallocated { id });
        }
        Ok(self.mapped.slot(id)?.get_acquire())
    }

    /// A handle on counter `id`'s value for a reader's hot loop: the slot is resolved
    /// once, here, with the checks of [`CountersReader::read`], and each
    /// [`CounterValue::load_acquire`] after is one acquire load with no check, no
    /// allocation and no panic path. A publisher reads its `publication.limit` this way
    /// on every claim (`spec/log-buffer.md`, "Claim, commit and abort"; ADR 0013). The
    /// record may be freed and reused under the handle, as any counter may; the owner of
    /// the instance the counter belongs to is told so by the driver first (ADR 0019).
    pub fn value(&self, id: u32) -> Result<CounterValue, CountersError> {
        if self.mapped.record(id)?.state()? != State::Allocated {
            return Err(CountersError::Unallocated { id });
        }
        let offset = self.mapped.fields.slot_offset(id)?;
        Ok(CounterValue {
            mapped: Arc::clone(&self.mapped),
            offset,
            id,
        })
    }
}

impl fmt::Debug for CountersReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CountersReader")
            .field("header", &self.mapped.fields)
            .finish()
    }
}
