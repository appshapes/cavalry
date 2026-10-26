//! The log buffer on disk: creating it, mapping it, the publisher's appender, the
//! subscriber's reader and the driver's duties, each over the cells of `cells`. Every
//! `unsafe` block that touches a mapping is in this module; `cells` and `layout` never
//! see a file.

use core::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use cavalry_proto::frame::{
    DataFrameHeader, Flags, FrameType, HEADER_LENGTH as FRAME_HEADER_LENGTH,
};
use memmap2::{Mmap, MmapMut, MmapOptions};

use crate::pretouch;

use super::LogError;
use super::cells::{FrameCells, MetaDataCells};
use super::layout::{
    self, ClaimRefused, HEADER_LENGTH, HeaderFields, LogType, META_DATA_LENGTH, NOT_ENDED,
    PARTITION_COUNT, Padding, Positions, Reserved, Rotation, Tail, aligned_length,
};

const FRAME_ALIGNMENT: u32 = cavalry_proto::frame::FRAME_ALIGNMENT as u32;
const FRAME_HEADER: u32 = FRAME_HEADER_LENGTH as u32;

/// The writable mapping of the driver and the publishers, or the read-only mapping of a
/// subscriber.
enum Map {
    Writer(MmapMut),
    Reader(Mmap),
}

/// A mapping with the header fields it was checked against. Every cell is a view at an
/// offset of `base`, and every `unsafe` block below relies on the same facts: the mapping
/// is exactly `fields.file_length()` bytes, checked in `new`, and every offset is derived
/// from `fields` or from a position or tail clamped to the term length; the base is
/// page-aligned, so a multiple of 32 keeps the 8-byte alignment the frame cells need and
/// 3 × term length, a multiple of 64 KiB, keeps the 128-byte alignment the metadata page
/// needs; and the cells are `#[repr(C)]` atomics and byte arrays in `UnsafeCell`s, valid
/// for every bit pattern, which other processes write only through the protocols `cells`
/// names.
struct Mapped {
    _map: Map,
    base: *const u8,
    length: usize,
    fields: HeaderFields,
    positions: Positions,
    writable: bool,
}

// SAFETY: `base` points into `_map`, which lives as long as `self`, and the only
// accesses through it are the atomics and the protocols of `cells`, whose types are
// `Sync`, and the body slices whose arguments are at `body` and `body_mut`; the pointer
// itself is never written after `new`.
unsafe impl Send for Mapped {}
// SAFETY: as above.
unsafe impl Sync for Mapped {}

impl Mapped {
    fn new(mut map: Map, fields: HeaderFields) -> Result<Self, LogError> {
        // The writer's pointer comes from `&mut [u8]`, so it keeps permission to write;
        // the map is never dereferenced again, so nothing invalidates it.
        let (base, length, writable) = match &mut map {
            Map::Writer(map) => (map.as_mut_ptr().cast_const(), map.len(), true),
            Map::Reader(map) => (map.as_ptr(), map.len(), false),
        };
        let needed = fields.file_length();
        if length as u64 != needed {
            return Err(LogError::Length {
                found: length as u64,
                needed,
            });
        }
        Ok(Self {
            _map: map,
            base,
            length,
            fields,
            positions: fields.positions(),
            writable,
        })
    }

    fn term_length(&self) -> u32 {
        self.fields.term_length()
    }

    fn meta(&self) -> &MetaDataCells {
        let offset = self.fields.meta_data_offset() as usize;
        debug_assert!(offset + META_DATA_LENGTH == self.length);
        // SAFETY: the mapping is exactly `file_length()` bytes (`new`), so the 4096 bytes
        // at 3 × term length are the mapping's last 4096; that offset is a multiple of
        // 64 KiB from a page-aligned base, which the page's 128-byte alignment needs;
        // `MetaDataCells` is atomics and bytes in `UnsafeCell`s; the reference lives no
        // longer than `self`, which owns the map.
        unsafe { &*self.base.add(offset).cast::<MetaDataCells>() }
    }

    /// The frame cells at `term_offset` in `partition`: a multiple of 32 at least 32
    /// below the term length, in a partition below 3, which every caller derives from a
    /// position or a clamped tail and has checked fits.
    fn frame(&self, partition: usize, term_offset: u32) -> &FrameCells {
        debug_assert!(partition < PARTITION_COUNT);
        debug_assert!(
            term_offset.is_multiple_of(FRAME_ALIGNMENT)
                && term_offset + FRAME_HEADER <= self.term_length()
        );
        let offset = partition * self.term_length() as usize + term_offset as usize;
        // SAFETY: `partition` is below 3 and `term_offset` a multiple of 32 at least 32
        // below the term length (the callers' checks, asserted above in debug builds), so
        // the 32 bytes are inside the term, which is inside the mapping; a multiple of 32
        // from a page-aligned base keeps the 8-byte alignment; `FrameCells` is an atomic
        // and bytes in an `UnsafeCell`; the reference lives no longer than `self`.
        unsafe { &*self.base.add(offset).cast::<FrameCells>() }
    }

    /// The body of the committed frame at `term_offset` with `frame_length`, which the
    /// caller acquired through the frame's word and checked with `parse_within` against
    /// the bytes left in the term.
    fn body(&self, partition: usize, term_offset: u32, frame_length: u32) -> &[u8] {
        let start =
            partition * self.term_length() as usize + term_offset as usize + FRAME_HEADER_LENGTH;
        let length = (frame_length - FRAME_HEADER) as usize;
        debug_assert!(term_offset + frame_length <= self.term_length());
        // SAFETY: `parse_within` checked `frame_length` against the bytes from the frame
        // to the end of the term, so the body is inside the term and the mapping; the
        // publisher released the positive length after writing the body and the caller
        // acquired it, so the bytes are complete; nothing rewrites a committed frame
        // until the driver cleans it, which happens only below every subscriber's
        // position, after this reader stored its own with release. A process that writes
        // here regardless breaks the spec; ADR 0010 accepts the driver's directory as
        // the trust boundary.
        unsafe { core::slice::from_raw_parts(self.base.add(start), length) }
    }

    /// The bytes of a block of committed frames: `length` bytes from `term_offset` in
    /// `partition`, which the caller scanned frame by frame, acquiring each frame's
    /// positive length through its word, headers included.
    fn block(&self, partition: usize, term_offset: u32, length: u32) -> &[u8] {
        let start = partition * self.term_length() as usize + term_offset as usize;
        debug_assert!(
            term_offset.is_multiple_of(FRAME_ALIGNMENT)
                && term_offset + length <= self.term_length()
        );
        // SAFETY: the caller acquired a positive length at `term_offset` and at the start
        // of every frame up to `term_offset + length`, each stored by its publisher with
        // release after the header's other 24 bytes and the body were written
        // (`cells::FrameCells`), so every byte of the range is complete, the eight bytes
        // of each word included, which the acquire load ordered before this read (the
        // loom model `a_block_scan_reads_the_word_it_acquired_without_a_race`); nothing
        // rewrites a committed frame until the driver cleans it, which happens only
        // below every reader's position, after this reader stored its own with release
        // (`spec/log-buffer.md`, "The driver"). The range is inside the term, which is
        // inside the mapping. A process that writes here regardless breaks the spec;
        // ADR 0010 accepts the driver's directory as the trust boundary.
        unsafe { core::slice::from_raw_parts(self.base.add(start), length as usize) }
    }

    /// The body of the frame the caller has just claimed at `term_offset` with
    /// `frame_length`, to write into until it commits.
    #[allow(clippy::mut_from_ref)]
    fn body_mut(&self, partition: usize, term_offset: u32, frame_length: u32) -> &mut [u8] {
        debug_assert!(self.writable);
        let start =
            partition * self.term_length() as usize + term_offset as usize + FRAME_HEADER_LENGTH;
        let length = (frame_length - FRAME_HEADER) as usize;
        debug_assert!(term_offset + aligned_length(frame_length) <= self.term_length());
        // SAFETY: the publisher reserved the aligned frame length at `term_offset` with
        // the tail's fetch-and-add, or owns the tail (type 1), so no other publisher
        // writes these bytes; readers read a body only after acquiring a positive length,
        // which the claim's commit releases after the writes through this slice; the
        // slice lives as long as the `BufferClaim`, which publishes or aborts the frame
        // when it ends. The driver's unblock rewrites an abandoned claim only after the
        // unblock timeout, a bound in time. The mapping is writable (asserted above).
        unsafe { core::slice::from_raw_parts_mut(self.base.cast_mut().add(start), length) }
    }

    fn require_writable(&self) -> Result<(), LogError> {
        if self.writable {
            Ok(())
        } else {
            Err(LogError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the log buffer was opened read-only; open it writable to publish or to run the driver's duties",
            )))
        }
    }

    /// The active term count and the active tail, as a publisher or the driver loads
    /// them, with the spec's `tail` check.
    fn active(&self) -> Result<(i32, Tail), LogError> {
        let meta = self.meta();
        let count = meta.active_term_count.load();
        let tail = meta.tails[Positions::partition_of_count(count)].load();
        let expected = self.positions.initial_term_id().wrapping_add(count);
        // Built out only to measure the check's cost (ADR 0010; `just bench-unchecked`).
        #[cfg(not(cavalry_unchecked))]
        if tail.term_id() != expected {
            return Err(LogError::Tail {
                found: tail.term_id(),
                expected,
            });
        }
        #[cfg(cavalry_unchecked)]
        let _ = expected;
        Ok((count, tail))
    }

    /// Rotation, as `spec/log-buffer.md` says: compare-and-swap the next partition's
    /// tail while it still holds the term three back, then the active term count.
    fn rotate(&self, rotation: &Rotation, term_count: i32) -> bool {
        let meta = self.meta();
        let tails = &meta.tails[rotation.next_partition];
        loop {
            let found = tails.load();
            if found.term_id() != rotation.expected_term_id {
                break;
            }
            if tails.compare_exchange(found, rotation.next_tail).is_ok() {
                break;
            }
        }
        meta.active_term_count
            .compare_exchange(term_count, rotation.next_term_count)
    }

    /// Writes a padding frame over `padding` in `partition` of term `term_id`, by the
    /// claim protocol with type 0.
    fn pad(
        &self,
        partition: usize,
        term_id: i32,
        padding: Padding,
        default_header: &[u8; FRAME_HEADER_LENGTH],
    ) {
        let claimed = layout::claim_header(
            default_header,
            padding.term_offset,
            padding.frame_length,
            term_id,
            0,
        );
        let frame = self.frame(partition, padding.term_offset);
        frame.claim(&claimed);
        frame.publish(layout::frame_word(&layout::aborted_header(&claimed)));
    }
}

/// Why a claim or an offer did not write: the spec's four results that are not a claim,
/// and the caller's own error, a message longer than the log allows. The caller decides
/// what to do: wait on `BackPressured`, retry at once on `AdminAction`, close on
/// `MaxPositionExceeded`. Nothing was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaimError {
    /// No subscriber has a position on this log, and the position is at the limit.
    NotConnected,
    /// The slowest subscriber is a window behind: try again later.
    BackPressured,
    /// A rotation happened: try again at once, in the next term.
    AdminAction,
    /// The stream reached the largest position its term length allows: close the
    /// publication and create a new one.
    MaxPositionExceeded,
    /// A message longer than the log allows: MTU length − 32 for a claim, the smaller of
    /// term length ÷ 8 and 16 MiB for an offer.
    TooLong {
        /// The length asked for.
        found: usize,
        /// The longest this log allows.
        limit: usize,
    },
}

impl ClaimError {
    /// The result's name: `not-connected`, `back-pressured`, `admin-action`,
    /// `max-position-exceeded` or `too-long`.
    pub const fn reason(self) -> &'static str {
        match self {
            ClaimError::NotConnected => "not-connected",
            ClaimError::BackPressured => "back-pressured",
            ClaimError::AdminAction => "admin-action",
            ClaimError::MaxPositionExceeded => "max-position-exceeded",
            ClaimError::TooLong { .. } => "too-long",
        }
    }
}

impl From<ClaimRefused> for ClaimError {
    fn from(refused: ClaimRefused) -> Self {
        match refused {
            ClaimRefused::NotConnected => ClaimError::NotConnected,
            ClaimRefused::BackPressured => ClaimError::BackPressured,
            ClaimRefused::AdminAction => ClaimError::AdminAction,
            ClaimRefused::MaxPositionExceeded => ClaimError::MaxPositionExceeded,
        }
    }
}

impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DOC: &str = "docs/reference/log-buffer.md#claim-commit-and-abort";
        let reason = self.reason();
        match self {
            ClaimError::NotConnected => write!(
                f,
                "claim refused ({reason}): no subscriber has a position on this log; nothing is lost, \
                 try again when one connects; see {DOC}"
            ),
            ClaimError::BackPressured => write!(
                f,
                "claim refused ({reason}): the slowest subscriber is a window behind; try again later, \
                 and read subscription.position to find it; see {DOC}"
            ),
            ClaimError::AdminAction => write!(
                f,
                "claim refused ({reason}): the term rotated; try again at once; see {DOC}"
            ),
            ClaimError::MaxPositionExceeded => write!(
                f,
                "claim refused ({reason}): the stream reached the largest position its term length \
                 allows; close the publication and create a new one, with a longer term to make this \
                 rarer; see {DOC}"
            ),
            ClaimError::TooLong { found, limit } => write!(
                f,
                "claim refused ({reason}): a message of {found} bytes, this log allows {limit}; split the \
                 message, or create the publication with a longer term or MTU; see {DOC}"
            ),
        }
    }
}

impl std::error::Error for ClaimError {}

/// What the driver's unblock did (`spec/log-buffer.md`, "The driver").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnblockOutcome {
    /// The frame at the consumer position is committed, or a claim there is still in
    /// progress with nothing beyond it: nothing to do.
    NoAction,
    /// A padding frame now covers the stale claim or the zero gap.
    Unblocked,
    /// A padding frame covers the rest of the term, and the stream rotated.
    UnblockedToEnd,
    /// The consumer position was the first byte of the term after the active one: the
    /// stream rotated.
    Rotated,
}

/// One log buffer: the mapping the driver created or a client opened. `Send` and
/// `Sync`: a [`TermAppender`] or a [`TermReader`] made from it keeps the mapping alive on
/// its own, and the driver's duties are methods here.
pub struct LogBuffers {
    mapped: Arc<Mapped>,
}

impl LogBuffers {
    /// Creates `dir/publications/<correlation id>.logbuffer` for `fields` and publishes
    /// it: sizes the file, sets tail 0 to (initial term id, 0) and tails 1 and 2 to the
    /// terms two and one back, the end of stream position to not ended, is connected
    /// to 0, and writes the header magic-last (`spec/log-buffer.md`, "Metadata page").
    /// A file already at that path is [`LogError::Io`].
    pub fn create(dir: &Path, fields: &HeaderFields) -> Result<Self, LogError> {
        let path = dir.join(fields.file_name());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        let length = fields.file_length();
        file.set_len(length)?;
        // SAFETY: memmap2 marks mapping a file `unsafe` because another process could
        // truncate it under the mapping and turn a load into a fault. This call created
        // the file, in the driver's directory, the trust boundary every shared layout
        // assumes (ADR 0010 bounds the values a peer writes, not whether the file
        // exists); a truncation is a fault, never silent misbehaviour.
        let map = unsafe { MmapOptions::new().len(length as usize).map_mut(&file)? };
        pretouch::writable(&map)?;
        let mapped = Arc::new(Mapped::new(Map::Writer(map), *fields)?);
        let meta = mapped.meta();
        let initial = fields.initial_term_id();
        meta.tails[0].store(Tail::pack(initial, 0));
        for (i, tail) in meta.tails.iter().enumerate().skip(1) {
            tail.store(Tail::pack(
                initial
                    .wrapping_add(i as i32)
                    .wrapping_sub(PARTITION_COUNT as i32),
                0,
            ));
        }
        meta.stream.set_end_of_stream_position(NOT_ENDED);
        meta.stream.set_connected(false);
        meta.header.publish(fields);
        Ok(Self { mapped })
    }

    /// Opens the log of publication `correlation_id` under `dir`, writable for a
    /// publisher or the driver and read-only for a subscriber, and runs the spec's
    /// checks in the spec's order (`spec/log-buffer.md`, "What a reader rejects"): the
    /// file's size (`too-short`), the header read once from the file (`not-ready` to
    /// `length`), then the header again through the final mapping, which must equal the
    /// first read (`changed`).
    pub fn open(dir: &Path, correlation_id: i64, writable: bool) -> Result<Self, LogError> {
        let path = dir.join(layout::file_name(correlation_id));
        let mut file = OpenOptions::new().read(true).write(writable).open(&path)?;
        let file_length = file.metadata()?.len();
        if file_length < META_DATA_LENGTH as u64 {
            return Err(LogError::TooShort { given: file_length });
        }
        let mut probe = [0u8; HEADER_LENGTH];
        file.seek(SeekFrom::Start(file_length - META_DATA_LENGTH as u64))?;
        file.read_exact(&mut probe)?;
        let first = layout::parse_header(&probe, file_length)?;
        // SAFETY: as at `create`: memmap2 marks the mapping `unsafe` because another
        // process could truncate the file underneath it, and the driver's directory is
        // the trust boundary; a truncation faults, never lies.
        let map = if writable {
            let map = unsafe {
                MmapOptions::new()
                    .len(file_length as usize)
                    .map_mut(&file)?
            };
            pretouch::writable(&map)?;
            Map::Writer(map)
        } else {
            // SAFETY: as above.
            let map = unsafe { MmapOptions::new().len(file_length as usize).map(&file)? };
            pretouch::readable(&map)?;
            Map::Reader(map)
        };
        let mapped = Mapped::new(map, first)?;
        let second = mapped.meta().header.read(file_length)?;
        if second != first {
            return Err(LogError::Changed);
        }
        Ok(Self {
            mapped: Arc::new(mapped),
        })
    }

    /// The header's fields.
    pub fn header(&self) -> HeaderFields {
        self.mapped.fields
    }

    /// The position arithmetic of this log.
    pub fn positions(&self) -> Positions {
        self.mapped.positions
    }

    /// The tail of `partition`, taken modulo 3, with an acquire load.
    pub fn tail(&self, partition: usize) -> Tail {
        self.mapped.meta().tails[partition % PARTITION_COUNT].load()
    }

    /// The active term count, with an acquire load.
    pub fn active_term_count(&self) -> i32 {
        self.mapped.meta().active_term_count.load()
    }

    /// The producer position: the position of the active tail. [`LogError::Tail`] when
    /// the active tail's term id is not the active term's, which the driver reports.
    pub fn producer_position(&self) -> Result<i64, LogError> {
        let (_, tail) = self.mapped.active()?;
        let positions = self.mapped.positions;
        Ok(positions.position(tail.term_id(), tail.term_offset(positions.term_length())))
    }

    /// Whether at least one subscriber has a position on this log.
    pub fn is_connected(&self) -> bool {
        self.mapped.meta().stream.is_connected()
    }

    /// The driver sets whether a subscriber has a position on this log.
    pub fn set_connected(&self, connected: bool) -> Result<(), LogError> {
        self.mapped.require_writable()?;
        self.mapped.meta().stream.set_connected(connected);
        Ok(())
    }

    /// The end of stream position: [`layout::NOT_ENDED`] while the publication is open.
    pub fn end_of_stream_position(&self) -> i64 {
        self.mapped.meta().stream.end_of_stream_position()
    }

    /// The driver sets the end of stream position when the publication is closed.
    pub fn set_end_of_stream_position(&self, position: i64) -> Result<(), LogError> {
        self.mapped.require_writable()?;
        self.mapped
            .meta()
            .stream
            .set_end_of_stream_position(position);
        Ok(())
    }

    /// The driver's cleaning: zeroes the bytes from position `from` up to `to`, one
    /// term at a time, with the first eight bytes of the range written last with a
    /// release store, and returns the new clean position: the end of the range cleaned,
    /// which is `to` or the end of `from`'s term, whichever is first. The caller cleans
    /// only below the slowest subscriber's position, before it raises the limit
    /// (`spec/log-buffer.md`, "The driver").
    pub fn clean(&self, from: i64, to: i64) -> Result<i64, LogError> {
        self.mapped.require_writable()?;
        let positions = self.mapped.positions;
        if to <= from || from < 0 || from % i64::from(FRAME_ALIGNMENT) != 0 {
            return Ok(from);
        }
        let term_length = positions.term_length();
        let partition = positions.partition_of(from);
        let offset = positions.term_offset_of(from);
        let length = (to - from).min(i64::from(term_length - offset)) as u32 / FRAME_ALIGNMENT
            * FRAME_ALIGNMENT;
        if length == 0 {
            return Ok(from);
        }
        for cell in (offset + FRAME_ALIGNMENT..offset + length).step_by(FRAME_ALIGNMENT as usize) {
            self.mapped.frame(partition, cell).clean_rest();
        }
        self.mapped.frame(partition, offset).clean_first();
        Ok(from + i64::from(length))
    }

    /// The driver's unblocking at `consumer_position`, the slowest subscriber's position
    /// that has not moved for the unblock timeout (`spec/log-buffer.md`, "The driver"):
    /// a stale claim becomes a padding frame, a gap of zeros before a later frame
    /// becomes one, a gap to the end of the term becomes one and the stream rotates, and
    /// a consumer position at the start of the next term rotates.
    pub fn unblock(&self, consumer_position: i64) -> Result<UnblockOutcome, LogError> {
        self.mapped.require_writable()?;
        let positions = self.mapped.positions;
        let term_length = positions.term_length();
        if consumer_position < 0 || consumer_position % i64::from(FRAME_ALIGNMENT) != 0 {
            return Err(LogError::Position {
                found: consumer_position,
                current: 0,
                limit: positions.max_position(),
            });
        }
        let blocked_count = positions.term_count_of(consumer_position) as i32;
        let blocked_offset = positions.term_offset_of(consumer_position);
        let meta = self.mapped.meta();
        let active = meta.active_term_count.load();
        let default_header = self.mapped.fields.default_frame_header();
        if active == blocked_count.wrapping_sub(1) && blocked_offset == 0 {
            let term_id = meta.tails[Positions::partition_of_count(active)]
                .load()
                .term_id();
            self.mapped.rotate(&rotation_after(active, term_id), active);
            return Ok(UnblockOutcome::Rotated);
        }
        let partition = Positions::partition_of_count(blocked_count);
        let raw = meta.tails[partition].load();
        let term_id = raw.term_id();
        let tail_offset = raw.term_offset(term_length);
        let first = self.mapped.frame(partition, blocked_offset);
        let length = layout::frame_word_length(first.word());
        if length < 0 {
            let magnitude = length.checked_neg().and_then(|m| u32::try_from(m).ok());
            let Some(padding) =
                magnitude.filter(|&m| m >= FRAME_HEADER && m <= term_length - blocked_offset)
            else {
                return Err(LogError::Frame(cavalry_proto::frame::FrameError::Length {
                    found: i64::from(length),
                    limit: (term_length - blocked_offset) as usize,
                }));
            };
            self.mapped.pad(
                partition,
                term_id,
                Padding {
                    term_offset: blocked_offset,
                    frame_length: padding,
                },
                &default_header,
            );
            return Ok(UnblockOutcome::Unblocked);
        }
        if length > 0 {
            return Ok(UnblockOutcome::NoAction);
        }
        let mut current = blocked_offset + FRAME_ALIGNMENT;
        while current < tail_offset {
            if layout::frame_word_length(self.mapped.frame(partition, current).word()) != 0 {
                if self.zeroed_between(partition, blocked_offset, current) {
                    let padding = Padding {
                        term_offset: blocked_offset,
                        frame_length: current - blocked_offset,
                    };
                    self.mapped
                        .pad(partition, term_id, padding, &default_header);
                    return Ok(UnblockOutcome::Unblocked);
                }
                return Ok(UnblockOutcome::NoAction);
            }
            current += FRAME_ALIGNMENT;
        }
        if current == term_length && layout::frame_word_length(first.word()) == 0 {
            let padding = Padding {
                term_offset: blocked_offset,
                frame_length: term_length - blocked_offset,
            };
            self.mapped
                .pad(partition, term_id, padding, &default_header);
            self.mapped
                .rotate(&rotation_after(blocked_count, term_id), blocked_count);
            return Ok(UnblockOutcome::UnblockedToEnd);
        }
        Ok(UnblockOutcome::NoAction)
    }

    /// Whether every frame word from `from` up to `until` in `partition` is 0.
    fn zeroed_between(&self, partition: usize, from: u32, until: u32) -> bool {
        (from..until)
            .step_by(FRAME_ALIGNMENT as usize)
            .all(|offset| {
                layout::frame_word_length(self.mapped.frame(partition, offset).word()) == 0
            })
    }

    /// A publisher's appender on this log, which must be writable.
    pub fn appender(&self) -> Result<TermAppender, LogError> {
        TermAppender::new(Arc::clone(&self.mapped))
    }

    /// A subscriber's reader on this log, starting at `position`, a multiple of 32 from
    /// 0 to below the largest position; otherwise [`LogError::Position`].
    pub fn reader(&self, position: i64) -> Result<TermReader, LogError> {
        TermReader::new(Arc::clone(&self.mapped), position)
    }
}

impl fmt::Debug for LogBuffers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogBuffers")
            .field("header", &self.mapped.fields)
            .finish()
    }
}

/// The rotation out of term `term_id`, the active term with count `term_count`.
fn rotation_after(term_count: i32, term_id: i32) -> Rotation {
    let next_term_id = term_id.wrapping_add(1);
    let next_term_count = term_count.wrapping_add(1);
    Rotation {
        next_partition: Positions::partition_of_count(next_term_count),
        expected_term_id: next_term_id.wrapping_sub(PARTITION_COUNT as i32),
        next_tail: Tail::pack(next_term_id, 0),
        next_term_count,
    }
}

/// The one publisher's copy of the tail in an exclusive log (type 1).
#[derive(Clone, Copy, Debug)]
struct ExclusiveTail {
    term_count: i32,
    term_id: i32,
    term_offset: u32,
}

/// A publisher's handle on a log: claims frames in place and offers messages by copy
/// (`spec/log-buffer.md`, "Claim, commit and abort"). One per publishing thread; several
/// appenders on one concurrent log coordinate through the tail's fetch-and-add. The
/// handle is `Send`, borrows nothing from the [`LogBuffers`] and keeps the mapping alive.
pub struct TermAppender {
    mapped: Arc<Mapped>,
    default_header: [u8; FRAME_HEADER_LENGTH],
    exclusive: Option<ExclusiveTail>,
}

impl TermAppender {
    fn new(mapped: Arc<Mapped>) -> Result<Self, LogError> {
        mapped.require_writable()?;
        let default_header = mapped.fields.default_frame_header();
        let exclusive = match mapped.fields.log_type() {
            LogType::Concurrent => None,
            LogType::Exclusive => {
                let (term_count, tail) = mapped.active()?;
                Some(ExclusiveTail {
                    term_count,
                    term_id: tail.term_id(),
                    term_offset: tail.term_offset(mapped.term_length()),
                })
            }
        };
        Ok(Self {
            mapped,
            default_header,
            exclusive,
        })
    }

    /// The longest body one claim carries: MTU length − 32.
    pub fn max_payload_length(&self) -> usize {
        self.mapped.fields.max_payload_length() as usize
    }

    /// The longest message one offer carries: the smaller of term length ÷ 8 and 16 MiB.
    pub fn max_message_length(&self) -> usize {
        self.mapped.fields.max_message_length() as usize
    }

    /// The producer position as this appender sees it.
    pub fn position(&self) -> Result<i64, LogError> {
        let (_, tail) = self.active()?;
        let positions = self.mapped.positions;
        Ok(positions.position(tail.term_id(), tail.term_offset(positions.term_length())))
    }

    fn active(&self) -> Result<(i32, Tail), LogError> {
        match self.exclusive {
            Some(ex) => Ok((ex.term_count, Tail::pack(ex.term_id, ex.term_offset))),
            None => self.mapped.active(),
        }
    }

    /// Step 2: reserves `aligned` bytes and returns the tail before the reservation.
    fn reserve(&mut self, term_count: i32, aligned: u32) -> Tail {
        match &mut self.exclusive {
            Some(ex) => {
                let before = Tail::pack(ex.term_id, ex.term_offset);
                ex.term_offset = ex.term_offset.saturating_add(aligned);
                let partition = Positions::partition_of_count(term_count);
                self.mapped.meta().tails[partition].store(Tail::pack(ex.term_id, ex.term_offset));
                before
            }
            None => self.mapped.meta().tails[Positions::partition_of_count(term_count)]
                .fetch_add(aligned),
        }
    }

    /// Steps 1 to 3 for a reservation of `aligned` bytes: the term and offset the frame
    /// goes to, or the refusal.
    fn place(&mut self, aligned: u32, limit: i64) -> Result<(usize, i32, u32, i64), ClaimError> {
        let positions = self.mapped.positions;
        let (term_count, tail) = match self.active() {
            Ok(active) => active,
            Err(_) => return Err(ClaimError::AdminAction),
        };
        let is_connected = self.mapped.meta().stream.is_connected();
        layout::before_reserve(&positions, term_count, tail, aligned, limit, is_connected)?;
        let before = self.reserve(term_count, aligned);
        let partition = Positions::partition_of_count(term_count);
        match layout::after_reserve(&positions, term_count, before, aligned) {
            Reserved::Fits {
                term_id,
                term_offset,
                position_after,
            } => Ok((partition, term_id, term_offset, position_after)),
            Reserved::EndOfTerm {
                term_id,
                padding,
                rotation,
            } => {
                if let Some(padding) = padding {
                    self.mapped
                        .pad(partition, term_id, padding, &self.default_header);
                }
                self.mapped.rotate(&rotation, term_count);
                if let Some(ex) = &mut self.exclusive {
                    ex.term_count = rotation.next_term_count;
                    ex.term_id = rotation.next_tail.term_id();
                    ex.term_offset = 0;
                }
                Err(ClaimError::AdminAction)
            }
            Reserved::MaxPositionExceeded { term_id, padding } => {
                if let Some(padding) = padding {
                    self.mapped
                        .pad(partition, term_id, padding, &self.default_header);
                }
                Err(ClaimError::MaxPositionExceeded)
            }
        }
    }

    /// Claims a frame for a body of `length` bytes while the producer position is below
    /// `limit`, the publisher limit the driver maintains. The claim's body is written in
    /// place and published by [`BufferClaim::commit`]. The refusals are
    /// [`ClaimError`]'s; a `length` over [`TermAppender::max_payload_length`] is
    /// [`ClaimError::TooLong`] before anything is reserved.
    pub fn try_claim(&mut self, length: usize, limit: i64) -> Result<BufferClaim<'_>, ClaimError> {
        let max = self.max_payload_length();
        if length > max {
            return Err(ClaimError::TooLong {
                found: length,
                limit: max,
            });
        }
        let frame_length = length as u32 + FRAME_HEADER;
        let aligned = aligned_length(frame_length);
        let (partition, term_id, term_offset, position_after) = self.place(aligned, limit)?;
        let header =
            layout::claim_header(&self.default_header, term_offset, frame_length, term_id, 0);
        let frame = self.mapped.frame(partition, term_offset);
        frame.claim(&header);
        let body = self.mapped.body_mut(partition, term_offset, frame_length);
        Ok(BufferClaim {
            frame,
            body,
            header,
            position_after,
            done: false,
        })
    }

    /// Copies `message` in as one frame, or as consecutive fragments of
    /// [`TermAppender::max_payload_length`] bytes with the BEGIN flag on the first and
    /// END on the last, each committed in order, while the producer position is below
    /// `limit`; `reserved_value` goes in every fragment's header. Returns the position
    /// after the message. A message over [`TermAppender::max_message_length`] is
    /// [`ClaimError::TooLong`] before anything is reserved.
    pub fn offer(
        &mut self,
        message: &[u8],
        limit: i64,
        reserved_value: i64,
    ) -> Result<i64, ClaimError> {
        let max_payload = self.max_payload_length();
        let max_message = self.max_message_length();
        if message.len() > max_message {
            return Err(ClaimError::TooLong {
                found: message.len(),
                limit: max_message,
            });
        }
        if message.len() <= max_payload {
            let mut claim = self.try_claim(message.len(), limit)?;
            claim.body().copy_from_slice(message);
            claim.set_reserved_value(reserved_value);
            return Ok(claim.commit());
        }
        let full = message.len() / max_payload;
        let rest = message.len() % max_payload;
        let framed = full as u32 * (max_payload as u32 + FRAME_HEADER)
            + if rest > 0 {
                aligned_length(rest as u32 + FRAME_HEADER)
            } else {
                0
            };
        let (partition, term_id, mut term_offset, position_after) = self.place(framed, limit)?;
        let mut remaining = message;
        let mut flags = Flags::BEGIN;
        while !remaining.is_empty() {
            let take = remaining.len().min(max_payload);
            if take == remaining.len() {
                flags = flags | Flags::END;
            }
            let frame_length = take as u32 + FRAME_HEADER;
            let mut header = layout::claim_header(
                &self.default_header,
                term_offset,
                frame_length,
                term_id,
                reserved_value,
            );
            header[5] = flags.bits();
            let frame = self.mapped.frame(partition, term_offset);
            frame.claim(&header);
            self.mapped
                .body_mut(partition, term_offset, frame_length)
                .copy_from_slice(&remaining[..take]);
            frame.publish(layout::frame_word(&layout::committed_header(&header)));
            remaining = &remaining[take..];
            term_offset += aligned_length(frame_length);
            flags = Flags::NONE;
        }
        Ok(position_after)
    }
}

impl fmt::Debug for TermAppender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TermAppender")
            .field("correlation_id", &self.mapped.fields.correlation_id())
            .field("exclusive", &self.exclusive)
            .finish()
    }
}

/// A claimed frame: the body to write, then [`BufferClaim::commit`] or
/// [`BufferClaim::abort`]. Dropping a claim without either aborts it, so a panic between
/// claim and commit leaves a padding frame, not a stale claim; a publisher that dies
/// still leaves one, which the driver's unblock resolves.
pub struct BufferClaim<'a> {
    frame: &'a FrameCells,
    body: &'a mut [u8],
    header: [u8; FRAME_HEADER_LENGTH],
    position_after: i64,
    done: bool,
}

impl BufferClaim<'_> {
    /// The body, in place in the term: write the message here.
    pub fn body(&mut self) -> &mut [u8] {
        self.body
    }

    /// The stream position after this frame: what commit returns.
    pub fn position_after(&self) -> i64 {
        self.position_after
    }

    /// Sets the header's reserved value, carried to the subscriber unchanged; 0 unless
    /// set.
    pub fn set_reserved_value(&mut self, value: i64) {
        self.header[24..].copy_from_slice(&value.to_le_bytes());
        self.frame.set_reserved_value(value);
    }

    /// Publishes the frame with a release store of its positive length, and returns the
    /// position after it.
    pub fn commit(mut self) -> i64 {
        self.frame
            .publish(layout::frame_word(&layout::committed_header(&self.header)));
        self.done = true;
        self.position_after
    }

    /// Turns the frame into padding a reader skips, and returns the position after it.
    pub fn abort(mut self) -> i64 {
        self.frame
            .publish(layout::frame_word(&layout::aborted_header(&self.header)));
        self.done = true;
        self.position_after
    }
}

impl Drop for BufferClaim<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.frame
                .publish(layout::frame_word(&layout::aborted_header(&self.header)));
        }
    }
}

impl fmt::Debug for BufferClaim<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferClaim")
            .field("body_length", &self.body.len())
            .field("position_after", &self.position_after)
            .finish()
    }
}

/// A block of committed frames from one term, headers included, which
/// [`TermReader::block`] scanned: what a recorder writes into a segment file with one
/// positional write, so that a recorded position is the stream's position (ADR 0025).
#[derive(Clone, Copy)]
pub struct Block<'a> {
    /// The position of the block's first byte: the reader's position.
    pub position: i64,
    /// The bytes to write: every frame of the block with its header, or, for a padding
    /// frame, its 32-byte header alone, since a padding frame's body is zeros a writer
    /// skips.
    pub bytes: &'a [u8],
    /// The block's length in the stream: `bytes.len()`, or the padding frame's aligned
    /// length. The next block starts at `position + length`.
    pub length: usize,
    /// Whether the block is one padding frame.
    pub padding: bool,
}

impl Block<'_> {
    /// The position after the block, which the reader seeks to once the bytes are used.
    pub fn end(&self) -> i64 {
        self.position + self.length as i64
    }

    /// Whether nothing is committed at the position.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

impl fmt::Debug for Block<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Block")
            .field("position", &self.position)
            .field("length", &self.length)
            .field("padding", &self.padding)
            .finish()
    }
}

/// What one poll did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Poll {
    /// The reader's position after the poll.
    pub position: i64,
    /// How many data frames were delivered.
    pub fragments: usize,
}

/// A subscriber's reader on a log (`spec/log-buffer.md`, "Reading"): it keeps its
/// position, delivers committed data frames in order to a handler, skips padding, and
/// stops at a length of 0 or less or at the end of a term. The subscriber stores the
/// position it returns in its position counter with a release store. The handle is
/// `Send` and keeps the mapping alive.
pub struct TermReader {
    mapped: Arc<Mapped>,
    position: i64,
}

impl TermReader {
    fn new(mapped: Arc<Mapped>, position: i64) -> Result<Self, LogError> {
        let limit = mapped.positions.max_position();
        if position < 0 || position >= limit || position % i64::from(FRAME_ALIGNMENT) != 0 {
            return Err(LogError::Position {
                found: position,
                current: 0,
                limit,
            });
        }
        Ok(Self { mapped, position })
    }

    /// The reader's position: the start of the next frame it will read.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// Moves the reader to `position`, a multiple of 32 from the current position to the
    /// end of the current term inclusive; otherwise [`LogError::Position`].
    pub fn seek(&mut self, position: i64) -> Result<(), LogError> {
        let positions = self.mapped.positions;
        let current = self.position;
        let limit = current - i64::from(positions.term_offset_of(current))
            + i64::from(positions.term_length());
        if position < current || position > limit || position % i64::from(FRAME_ALIGNMENT) != 0 {
            return Err(LogError::Position {
                found: position,
                current,
                limit,
            });
        }
        self.position = position;
        Ok(())
    }

    /// Whether the reader's position is at or past the end of stream position: it has
    /// seen everything the publication wrote.
    pub fn is_end_of_stream(&self) -> bool {
        self.position >= self.mapped.meta().stream.end_of_stream_position()
    }

    /// Reads up to `limit` data frames from the position: each committed frame is
    /// checked as `spec/frame.md` says and a data frame's body is handed to `handler`
    /// with its header; a frame that fails a check is [`LogError::Frame`], the position
    /// stays at it, and nothing from it on is delivered. The read stops at a frame that
    /// is not committed or at the end of the term; the next poll continues, in the next
    /// term when this one is done.
    pub fn poll<F>(&mut self, limit: usize, mut handler: F) -> Result<Poll, LogError>
    where
        F: FnMut(&[u8], &DataFrameHeader),
    {
        let positions = self.mapped.positions;
        let term_length = positions.term_length();
        let partition = positions.partition_of(self.position);
        let mut offset = positions.term_offset_of(self.position);
        let term_begin = self.position - i64::from(offset);
        let mut fragments = 0;
        while fragments < limit && offset < term_length {
            let frame = self.mapped.frame(partition, offset);
            let word = frame.word();
            if layout::frame_word_length(word) <= 0 {
                break;
            }
            let bytes = frame.header(word);
            // The spec's checks, built out only to measure their cost (ADR 0010).
            #[cfg(not(cavalry_unchecked))]
            let header = DataFrameHeader::parse_within(&bytes, (term_length - offset) as usize)?;
            #[cfg(cavalry_unchecked)]
            let header = DataFrameHeader::decode_unchecked(&bytes);
            let frame_length = header.frame_length() as u32;
            if header.frame_type() == FrameType::Data {
                let body = self.mapped.body(partition, offset, frame_length);
                handler(body, &header);
                fragments += 1;
            }
            offset += aligned_length(frame_length);
            self.position = term_begin + i64::from(offset);
        }
        Ok(Poll {
            position: self.position,
            fragments,
        })
    }

    /// Scans a block of committed frames from the position, as Aeron's
    /// `TermBlockScanner` does behind `Image.rawPoll`: it loads each frame's length with
    /// an acquire load and checks the frame as `spec/frame.md` says, as [`TermReader::poll`]
    /// does, and takes whole frames in order until the next would carry the block past
    /// `limit` bytes, the term ends, or a frame is not yet committed. The first committed
    /// frame is always taken, so a block makes progress under any limit. A padding frame
    /// at the position is a block of its own, with its header as the bytes and its
    /// aligned length as the length; a padding frame after the first frame ends the
    /// block before it. A frame that fails a check is [`LogError::Frame`], and nothing is
    /// returned. The reader's position does not move: the caller seeks to [`Block::end`]
    /// once it has used the bytes, so a write that failed leaves the position where it
    /// was and the next call scans the same block again.
    pub fn block(&self, limit: usize) -> Result<Block<'_>, LogError> {
        let positions = self.mapped.positions;
        let term_length = positions.term_length();
        let partition = positions.partition_of(self.position);
        let start = positions.term_offset_of(self.position);
        let end = (start as usize)
            .saturating_add(limit)
            .min(term_length as usize) as u32;
        let mut offset = start;
        while offset < term_length && (offset == start || offset < end) {
            let frame = self.mapped.frame(partition, offset);
            let word = frame.word();
            if layout::frame_word_length(word) <= 0 {
                break;
            }
            let bytes = frame.header(word);
            // The spec's checks, built out only to measure their cost (ADR 0010).
            #[cfg(not(cavalry_unchecked))]
            let header = DataFrameHeader::parse_within(&bytes, (term_length - offset) as usize)?;
            #[cfg(cavalry_unchecked)]
            let header = DataFrameHeader::decode_unchecked(&bytes);
            let aligned = aligned_length(header.frame_length() as u32);
            if header.frame_type() != FrameType::Data {
                if offset == start {
                    return Ok(Block {
                        position: self.position,
                        bytes: self.mapped.block(partition, start, FRAME_HEADER),
                        length: aligned as usize,
                        padding: true,
                    });
                }
                break;
            }
            if offset != start && offset + aligned > end {
                break;
            }
            offset += aligned;
        }
        let length = offset - start;
        Ok(Block {
            position: self.position,
            bytes: self.mapped.block(partition, start, length),
            length: length as usize,
            padding: false,
        })
    }
}

impl fmt::Debug for TermReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TermReader")
            .field("correlation_id", &self.mapped.fields.correlation_id())
            .field("position", &self.position)
            .finish()
    }
}
