//! The command ring on disk: creating it, mapping it, the client's producer and the
//! driver's consumer, each over the cells of `cells`. Every `unsafe` block that touches a
//! mapping is in this module; `cells` and `layout` never see a file.

use core::fmt;
use std::fs::OpenOptions;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use memmap2::{Mmap, MmapMut, MmapOptions};

use super::CommandRingError;
use super::cells::{self, Claimed, TrailerCells};
use super::layout::{
    self, DRIVER_CLOSED, FILE_NAME, HEADER_LENGTH, HeaderFields, RECORD_HEADER_LENGTH, ReadStop,
    RecordHeader, TRAILER_LENGTH,
};
use crate::ring::{Words, now_nanos};
use crate::sync::AtomicU64;

/// The writable mapping of the driver and the clients, or the read-only mapping of
/// `doctor`.
enum Map {
    Writer(MmapMut),
    Reader(Mmap),
}

/// A mapping with the header fields it was checked against. Every cell is a view at an
/// offset of `base`, and every `unsafe` block below relies on the same facts: the
/// mapping is exactly `fields.file_length()` bytes, checked in `new`; a word index is a
/// multiple of 8 below the capacity, which the arithmetic of `layout` derives from
/// positions masked by capacity − 1 and lengths it checked; the base is page-aligned, so
/// a multiple of 8 keeps the words' alignment and the capacity, a multiple of 4096,
/// keeps the trailer's 128; and the cells are atomics and reserved bytes, valid for
/// every bit pattern, which other processes write only through the protocols `cells`
/// names.
struct Mapped {
    _map: Map,
    base: *const u8,
    length: usize,
    fields: HeaderFields,
    writable: bool,
}

// SAFETY: `base` points into `_map`, which lives as long as `self`, and the only
// accesses through it are the atomics of `cells` and the body slices whose arguments are
// at `body` and `body_mut`; the pointer itself is never written after `new`.
unsafe impl Send for Mapped {}
// SAFETY: as above.
unsafe impl Sync for Mapped {}

impl Mapped {
    fn new(mut map: Map, fields: HeaderFields) -> Result<Self, CommandRingError> {
        // The writer's pointer comes from `&mut [u8]`, so it keeps permission to write;
        // the map is never dereferenced again, so nothing invalidates it.
        let (base, length, writable) = match &mut map {
            Map::Writer(map) => (map.as_mut_ptr().cast_const(), map.len(), true),
            Map::Reader(map) => (map.as_ptr(), map.len(), false),
        };
        let needed = fields.file_length();
        if length as u64 != needed {
            return Err(CommandRingError::Length {
                found: length as u64,
                needed,
            });
        }
        Ok(Self {
            _map: map,
            base,
            length,
            fields,
            writable,
        })
    }

    fn trailer(&self) -> &TrailerCells {
        let offset = self.fields.trailer_offset() as usize;
        debug_assert!(offset + TRAILER_LENGTH == self.length);
        // SAFETY: the mapping is exactly `file_length()` bytes (`new`), so the 768 bytes
        // at the capacity are the mapping's last 768; the capacity is a multiple of
        // 4096 from a page-aligned base, which the trailer's 128-byte alignment needs;
        // `TrailerCells` is atomics and reserved bytes; the reference lives no longer
        // than `self`, which owns the map.
        unsafe { &*self.base.add(offset).cast::<TrailerCells>() }
    }

    /// The message of the committed record at `index` with `record_length`, which the
    /// caller acquired through the record's word and `RecordHeader::check` bounded to
    /// the ring.
    fn body(&self, index: u32, record_length: u32) -> &[u8] {
        let start = index as usize + RECORD_HEADER_LENGTH as usize;
        let length = (record_length - RECORD_HEADER_LENGTH) as usize;
        debug_assert!(index as usize + record_length as usize <= self.fields.capacity() as usize);
        // SAFETY: `RecordHeader::check` bounded the record to the ring, so the message is
        // inside the mapping; the producer released the positive length after writing
        // the message and the caller acquired it, so the bytes are complete; nothing
        // writes a committed record until the consumer clears it, which it does after
        // this slice is dropped (`cells::read`). A process that writes here regardless
        // breaks the spec; ADR 0010 accepts the driver's directory as the trust boundary.
        unsafe { core::slice::from_raw_parts(self.base.add(start), length) }
    }

    /// The message of the record the caller has just claimed at `index` with
    /// `record_length`, to write into until it commits.
    #[allow(clippy::mut_from_ref)]
    fn body_mut(&self, index: u32, record_length: u32) -> &mut [u8] {
        debug_assert!(self.writable);
        let start = index as usize + RECORD_HEADER_LENGTH as usize;
        let length = (record_length - RECORD_HEADER_LENGTH) as usize;
        debug_assert!(index as usize + record_length as usize <= self.fields.capacity() as usize);
        // SAFETY: the producer reserved the aligned record length at `index` with the
        // tail's compare-and-swap, so no other producer writes these bytes; the consumer
        // reads a message only after acquiring a positive length, which commit releases
        // after the writes through this slice; the slice lives as long as the `Claim`,
        // which commits or aborts when it ends. The driver's unblock rewrites an
        // abandoned claim's word, not its message, and only after the unblock timeout.
        // The mapping is writable (asserted above).
        unsafe { core::slice::from_raw_parts_mut(self.base.cast_mut().add(start), length) }
    }

    fn require_writable(&self) -> Result<(), CommandRingError> {
        if self.writable {
            Ok(())
        } else {
            Err(CommandRingError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the command ring was opened read-only; open it writable to produce or consume",
            )))
        }
    }
}

impl Words for Mapped {
    fn capacity(&self) -> u32 {
        self.fields.capacity()
    }

    fn word(&self, index: u32) -> &AtomicU64 {
        debug_assert!(index % 8 == 0 && index < self.fields.capacity());
        // SAFETY: `index` is a multiple of 8 below the capacity (the arithmetic of
        // `layout` masks every position by capacity − 1 and bounds every length it
        // adds; asserted above in debug builds), so the word is inside the ring, which
        // is inside the mapping, at the 8-byte alignment an `AtomicU64` needs from a
        // page-aligned base; an atomic is valid for every bit pattern; the reference
        // lives no longer than `self`.
        unsafe { &*self.base.add(index as usize).cast::<AtomicU64>() }
    }
}

/// One command ring: the mapping the driver created or a client opened. `Send` and
/// `Sync`: a [`Producer`] or a [`Consumer`] made from it keeps the mapping alive on its
/// own.
pub struct CommandRing {
    mapped: Arc<Mapped>,
}

impl CommandRing {
    /// Creates `dir/command-ring` for a ring of `capacity` bytes and publishes it: sizes
    /// the file, stores the driver's clock in the consumer heartbeat, and writes the
    /// header magic-last (`spec/command-ring.md`, "Trailer"). The capacity is a power of
    /// two from [`layout::MIN_CAPACITY`] to [`layout::MAX_CAPACITY`], or the error is
    /// [`CommandRingError::Capacity`]; a file already at that path is
    /// [`CommandRingError::Io`].
    pub fn create(dir: &Path, capacity: u32) -> Result<Self, CommandRingError> {
        let fields = HeaderFields::new(capacity, i64::from(std::process::id()), now_nanos())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.join(FILE_NAME))?;
        let length = fields.file_length();
        file.set_len(length)?;
        // SAFETY: memmap2 marks mapping a file `unsafe` because another process could
        // truncate it under the mapping and turn a load into a fault. This call created
        // the file, in the driver's directory, the trust boundary every shared layout
        // assumes (ADR 0010 bounds the values a peer writes, not whether the file
        // exists); a truncation is a fault, never silent misbehaviour.
        let map = unsafe { MmapOptions::new().len(length as usize).map_mut(&file)? };
        let mapped = Arc::new(Mapped::new(Map::Writer(map), fields)?);
        let trailer = mapped.trailer();
        trailer.heartbeat.store_release(fields.start_timestamp());
        trailer.publish(&fields);
        Ok(Self { mapped })
    }

    /// Opens `dir/command-ring`, writable for a client or the driver and read-only for
    /// `doctor`, and runs the spec's checks in the spec's order
    /// (`spec/command-ring.md`, "What a reader rejects"): the file's size (`too-short`),
    /// the header read once from the file at its last 768 bytes (`not-ready` to
    /// `length`), then the header again through the final mapping, which must equal the
    /// first read (`changed`).
    pub fn open(dir: &Path, writable: bool) -> Result<Self, CommandRingError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(dir.join(FILE_NAME))?;
        let file_length = file.metadata()?.len();
        if file_length < TRAILER_LENGTH as u64 {
            return Err(CommandRingError::TooShort { given: file_length });
        }
        let mut probe = [0u8; HEADER_LENGTH];
        file.seek(SeekFrom::Start(file_length - TRAILER_LENGTH as u64))?;
        file.read_exact(&mut probe)?;
        let first = layout::parse_header(&probe, file_length)?;
        // SAFETY: as at `create`: memmap2 marks the mapping `unsafe` because another
        // process could truncate the file underneath it, and the driver's directory is
        // the trust boundary; a truncation faults, never lies.
        let map = if writable {
            Map::Writer(unsafe {
                MmapOptions::new()
                    .len(file_length as usize)
                    .map_mut(&file)?
            })
        } else {
            // SAFETY: as above.
            Map::Reader(unsafe { MmapOptions::new().len(file_length as usize).map(&file)? })
        };
        let mapped = Mapped::new(map, first)?;
        let second = mapped.trailer().read(file_length)?;
        if second != first {
            return Err(CommandRingError::Changed);
        }
        Ok(Self {
            mapped: Arc::new(mapped),
        })
    }

    /// The header's fields.
    pub fn header(&self) -> HeaderFields {
        self.mapped.fields
    }

    /// The ring's capacity in bytes.
    pub fn capacity(&self) -> u32 {
        self.mapped.fields.capacity()
    }

    /// The longest message this ring carries: capacity ÷ 8.
    pub fn max_message_length(&self) -> usize {
        self.mapped.fields.max_message_length() as usize
    }

    /// A client's producer on this ring, which must be writable. `driver_timeout` is how
    /// old the consumer heartbeat may be before a claim reports `driver-timeout`.
    pub fn producer(&self, driver_timeout: Duration) -> Result<Producer, CommandRingError> {
        self.mapped.require_writable()?;
        Ok(Producer {
            mapped: Arc::clone(&self.mapped),
            driver_timeout: i64::try_from(driver_timeout.as_nanos()).unwrap_or(i64::MAX),
        })
    }

    /// The driver's consumer on this ring, which must be writable. One per ring: the
    /// head position has one writer.
    pub fn consumer(&self) -> Result<Consumer, CommandRingError> {
        self.mapped.require_writable()?;
        Ok(Consumer {
            mapped: Arc::clone(&self.mapped),
            rejected: None,
        })
    }

    /// The tail position, with an acquire load: where the next record is claimed.
    pub fn producer_position(&self) -> i64 {
        self.mapped.trailer().tail.load_acquire()
    }

    /// The head position, with an acquire load: where the next record is read.
    pub fn consumer_position(&self) -> i64 {
        self.mapped.trailer().head.load_acquire()
    }

    /// The consumer heartbeat, with an acquire load: the driver's clock in nanoseconds
    /// since the Unix epoch, or [`layout::DRIVER_CLOSED`].
    pub fn consumer_heartbeat(&self) -> i64 {
        self.mapped.trailer().heartbeat.load_acquire()
    }

    /// How many bytes the ring holds, tail − head, read until the head is stable and
    /// clamped to 0 and the capacity: what `doctor` reports as the fill.
    pub fn size(&self) -> u32 {
        let trailer = self.mapped.trailer();
        let mut after = trailer.head.load_acquire();
        loop {
            let before = after;
            let tail = trailer.tail.load_acquire();
            after = trailer.head.load_acquire();
            if after == before {
                let size = (tail - after).clamp(0, i64::from(self.capacity()));
                return size as u32;
            }
        }
    }
}

impl fmt::Debug for CommandRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandRing")
            .field("header", &self.mapped.fields)
            .finish()
    }
}

/// A client's handle on the ring: takes correlation ids and claims or writes records
/// (`spec/command-ring.md`, "Claim, commit and abort"). One per client thread; several
/// producers on one ring coordinate through the tail's compare-and-swap. The handle is
/// `Send`, borrows nothing from the [`CommandRing`] and keeps the mapping alive.
pub struct Producer {
    mapped: Arc<Mapped>,
    driver_timeout: i64,
}

impl Producer {
    /// The next correlation id: a fetch-and-add of 1 on the counter in the trailer.
    pub fn next_correlation_id(&self) -> i64 {
        self.mapped.trailer().correlation.fetch_add(1)
    }

    /// The consumer heartbeat, with an acquire load.
    pub fn consumer_heartbeat(&self) -> i64 {
        self.mapped.trailer().heartbeat.load_acquire()
    }

    /// The longest message this ring carries: capacity ÷ 8.
    pub fn max_message_length(&self) -> usize {
        self.mapped.fields.max_message_length() as usize
    }

    /// Checks `type_id` and `length` before anything else, as the spec says an API
    /// refuses them.
    fn check_request(&self, type_id: i32, length: usize) -> Result<(), CommandRingError> {
        if type_id < 1 {
            return Err(CommandRingError::Type { found: type_id });
        }
        let limit = self.max_message_length();
        if length > limit {
            return Err(CommandRingError::Message {
                found: length,
                limit,
            });
        }
        Ok(())
    }

    /// Claims a record for a message of `length` bytes and type `type_id`, with
    /// `now_nanos` the client's clock for the heartbeat check: the message is written
    /// in place through [`Claim::body`] and published by [`Claim::commit`]. A type below
    /// 1 is [`CommandRingError::Type`] and a message over
    /// [`Producer::max_message_length`] is [`CommandRingError::Message`], both before
    /// anything is read; a closed or stopped driver is [`CommandRingError::DriverClosed`]
    /// or [`CommandRingError::DriverTimeout`]; a ring with too few free bytes is
    /// [`CommandRingError::Full`]; a corrupt tail is [`CommandRingError::Tail`]. Nothing
    /// is written on any of them.
    pub fn try_claim(
        &mut self,
        type_id: i32,
        length: usize,
        now_nanos: i64,
    ) -> Result<Claim<'_>, CommandRingError> {
        self.check_request(type_id, length)?;
        let mapped = &*self.mapped;
        cells::check_driver(mapped.trailer(), now_nanos, self.driver_timeout)?;
        let claimed = cells::claim(mapped, mapped.trailer(), type_id, length)?;
        let body = mapped.body_mut(claimed.index, claimed.record_length);
        Ok(Claim {
            mapped,
            claimed,
            body,
            done: false,
        })
    }

    /// Copies `message` in as one record of type `type_id` and commits it, with the
    /// checks and errors of [`Producer::try_claim`].
    pub fn write(
        &mut self,
        type_id: i32,
        message: &[u8],
        now_nanos: i64,
    ) -> Result<(), CommandRingError> {
        let mut claim = self.try_claim(type_id, message.len(), now_nanos)?;
        claim.body().copy_from_slice(message);
        claim.commit();
        Ok(())
    }
}

impl fmt::Debug for Producer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Producer")
            .field("driver_timeout", &self.driver_timeout)
            .finish()
    }
}

/// A claimed record: the message to write, then [`Claim::commit`] or [`Claim::abort`].
/// Dropping a claim without either aborts it, so a panic between claim and commit
/// leaves a padding record, not a stale claim; a client that dies still leaves one,
/// which the driver's unblock resolves.
pub struct Claim<'a> {
    mapped: &'a Mapped,
    claimed: Claimed,
    body: &'a mut [u8],
    done: bool,
}

impl Claim<'_> {
    /// The message, in place in the ring: write the command here.
    pub fn body(&mut self) -> &mut [u8] {
        self.body
    }

    /// The record's index in the ring.
    pub fn index(&self) -> u32 {
        self.claimed.index
    }

    /// Publishes the record with one release store of its positive length and type.
    pub fn commit(mut self) {
        cells::commit(self.mapped, &self.claimed);
        self.done = true;
    }

    /// Turns the record into padding the consumer skips, with one release store.
    pub fn abort(mut self) {
        cells::abort(self.mapped, &self.claimed);
        self.done = true;
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if !self.done {
            cells::abort(self.mapped, &self.claimed);
        }
    }
}

impl fmt::Debug for Claim<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claim")
            .field("index", &self.claimed.index)
            .field("message_length", &self.body.len())
            .finish()
    }
}

/// What one read did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Read {
    /// How many messages were delivered.
    pub messages: usize,
    /// How many bytes were consumed and cleared, padding included.
    pub bytes: u32,
    /// Why the read stopped.
    pub stop: ReadStop,
}

/// What the driver's unblock did (`spec/command-ring.md`, "Unblocking").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnblockOutcome {
    /// Nothing was blocked: the ring is empty, or a committed record waits.
    NoAction,
    /// A padding record now covers the stale claim or the gap.
    Unblocked {
        /// The padding record's index.
        index: u32,
        /// The padding record's length.
        length: u32,
    },
}

/// What corrupted the ring, once the consumer found it: from then on every read and
/// unblock reports it without reading the ring (`spec/command-ring.md`, "Reading" step
/// 2, "Unblocking" step 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rejection {
    /// A record outside the spec's bounds, at this index.
    Record {
        /// The record's index in the ring.
        index: u32,
        /// The header found.
        header: RecordHeader,
    },
    /// A tail outside its bound against the head.
    Tail {
        /// The tail found.
        tail: i64,
        /// The head it was checked against.
        head: i64,
    },
}

impl Rejection {
    fn error(self) -> CommandRingError {
        match self {
            Rejection::Record { index, header } => CommandRingError::Record {
                index,
                length: header.length(),
                type_id: header.type_id(),
            },
            Rejection::Tail { tail, head } => CommandRingError::Tail { tail, head },
        }
    }
}

/// The driver's handle on the ring (`spec/command-ring.md`, "Reading", "Unblocking",
/// "Heartbeat and correlation ids"): reads records in order and clears them, unblocks a
/// claim a dead client left, and stores the heartbeat. One per ring. The handle is
/// `Send` and keeps the mapping alive.
pub struct Consumer {
    mapped: Arc<Mapped>,
    rejected: Option<Rejection>,
}

impl Consumer {
    fn corrupt(&self) -> Result<(), CommandRingError> {
        match self.rejected {
            Some(rejection) => Err(rejection.error()),
            None => Ok(()),
        }
    }

    /// Reads up to `limit` messages from the head: each committed record's type and
    /// message are given to `handler` in place, padding is skipped, the bytes read are
    /// zeroed and the head is released. The read stops at an unclaimed or claimed
    /// record, at the limit or at the end of the ring, which [`Read::stop`] names. A
    /// record outside the spec's bounds is [`CommandRingError::Record`]: the records
    /// before it were delivered and cleared, nothing from it on is touched, and every
    /// later read and unblock returns the same error without reading the ring, as they
    /// do after a [`CommandRingError::Tail`] an unblock found.
    pub fn read<F>(&mut self, limit: usize, mut handler: F) -> Result<Read, CommandRingError>
    where
        F: FnMut(i32, &[u8]),
    {
        self.corrupt()?;
        let mapped = &*self.mapped;
        let plan = cells::read(mapped, mapped.trailer(), limit, |index, header| {
            let body = mapped.body(index, header.length().unsigned_abs());
            handler(header.type_id(), body);
        });
        if let Some((index, header)) = plan.rejected {
            self.rejected = Some(Rejection::Record { index, header });
            self.corrupt()?;
        }
        Ok(Read {
            messages: plan.messages,
            bytes: plan.bytes_read,
            stop: plan.stop,
        })
    }

    /// The driver's unblock, run when the head has not moved for the unblock timeout
    /// while the tail is beyond it: a stale claim or a gap of zeros becomes a padding
    /// record. A tail outside its bound is [`CommandRingError::Tail`] and a stale claim
    /// outside the record bounds is [`CommandRingError::Record`]; both leave the ring
    /// corrupt, and every later read and unblock returns the same error without
    /// reading the ring.
    pub fn unblock(&mut self) -> Result<UnblockOutcome, CommandRingError> {
        self.corrupt()?;
        let mapped = &*self.mapped;
        match cells::unblock(mapped, mapped.trailer()) {
            Ok(Some(padding)) => Ok(UnblockOutcome::Unblocked {
                index: padding.index,
                length: padding.length,
            }),
            Ok(None) => Ok(UnblockOutcome::NoAction),
            Err(CommandRingError::Record {
                index,
                length,
                type_id,
            }) => {
                self.rejected = Some(Rejection::Record {
                    index,
                    header: RecordHeader::new(length, type_id),
                });
                self.corrupt().map(|()| UnblockOutcome::NoAction)
            }
            Err(CommandRingError::Tail { tail, head }) => {
                self.rejected = Some(Rejection::Tail { tail, head });
                self.corrupt().map(|()| UnblockOutcome::NoAction)
            }
            Err(error) => Err(error),
        }
    }

    /// What corrupted the ring, once a read or an unblock found it.
    pub fn rejected(&self) -> Option<Rejection> {
        self.rejected
    }

    /// Stores the driver's clock, `now_nanos` since the Unix epoch, in the consumer
    /// heartbeat with a release store; at least once per heartbeat interval.
    pub fn store_heartbeat(&self, now_nanos: i64) {
        self.mapped.trailer().heartbeat.store_release(now_nanos);
    }

    /// Stores −1 in the consumer heartbeat: the driver closed, and a client that claims
    /// from now on gets `driver-closed`.
    pub fn close(&self) {
        self.mapped.trailer().heartbeat.store_release(DRIVER_CLOSED);
    }

    /// The tail position, with an acquire load.
    pub fn producer_position(&self) -> i64 {
        self.mapped.trailer().tail.load_acquire()
    }

    /// The head position: the consumer's own.
    pub fn consumer_position(&self) -> i64 {
        self.mapped.trailer().head.load_own()
    }
}

impl fmt::Debug for Consumer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Consumer")
            .field("head", &self.consumer_position())
            .field("rejected", &self.rejected)
            .finish()
    }
}
