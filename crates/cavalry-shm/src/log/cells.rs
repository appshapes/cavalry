//! The metadata page and the frames as cells: the atomics the protocols of
//! `spec/log-buffer.md` need, around the plain bytes of [`layout`]. As in
//! `counters::cells`, nothing here is constructed in production: `file` casts an offset
//! of the mapping to one of these, so every struct is `#[repr(C)]` at the spec's
//! offsets and holds nothing but atomics and byte arrays in `UnsafeCell`s, valid for
//! every bit pattern. Under `cfg(loom)` the atomics are loom's and the tests at the
//! bottom check the claim protocol and the tail; `just loom` runs them.

// Under loom, `file` is not compiled (loom's atomics have another size), so the parts of
// the cells only `file` calls are unused there.
#![cfg_attr(loom, allow(dead_code))]

use cavalry_proto::frame::HEADER_LENGTH as FRAME_HEADER_LENGTH;

use super::LogError;
use super::layout::{
    self, HEADER_LENGTH, HeaderFields, MAGIC, META_DATA_LENGTH, PARTITION_COUNT, SLOT_LENGTH, Tail,
};
use crate::sync::{AtomicI32, AtomicI64, AtomicU64, Ordering, UnsafeCell};

/// The header: the magic as the atomic the creator stores last and a reader loads
/// first, then the other fields as bytes. The same protocol as the counters file's.
#[repr(C)]
pub(crate) struct HeaderCells {
    magic: AtomicU64,
    rest: UnsafeCell<[u8; HEADER_LENGTH - 8]>,
}

// SAFETY: `rest` is written once, by the creator, before its release store of `magic`
// (`publish`), and read only after an acquire load of `magic` found the spec's value
// (`read`), so no two parties touch the bytes at once; `magic` is an atomic (spec,
// "Header").
unsafe impl Sync for HeaderCells {}

impl HeaderCells {
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            magic: AtomicU64::new(0),
            rest: UnsafeCell::new([0; HEADER_LENGTH - 8]),
        }
    }

    /// The creator's protocol: the fields after the magic, then the magic with a release
    /// store. Everything else in the metadata page was written before this call.
    pub(crate) fn publish(&self, fields: &HeaderFields) {
        let rest = layout::encode_header_rest(fields);
        // SAFETY: the creator is the only writer of a header and publishes once, on a
        // file it created (`file::LogBuffers::create`); no reader reads `rest` before the
        // release store below, which `read` acquires first.
        self.rest.with_mut(|p| unsafe { p.write(rest) });
        self.magic
            .store(u64::from_ne_bytes(MAGIC), Ordering::Release);
    }

    /// A reader's protocol: the magic with an acquire load, then the rest, then the
    /// spec's checks for a file of `file_length` bytes, in order.
    pub(crate) fn read(&self, file_length: u64) -> Result<HeaderFields, LogError> {
        let magic = self.magic.load(Ordering::Acquire).to_ne_bytes();
        layout::check_magic(magic)?;
        // SAFETY: the acquire load above saw the magic the creator stored with release
        // after writing `rest`, and the header never changes after that (spec,
        // "Header"), so the bytes are complete and nobody writes them.
        let rest = self.rest.with(|p| unsafe { p.read() });
        layout::parse_header_rest(&rest, file_length)
    }
}

/// One tail counter in its 128-byte slot (ADR 0011).
#[repr(C, align(128))]
pub(crate) struct TailCells {
    tail: AtomicI64,
    reserved: UnsafeCell<[u8; SLOT_LENGTH - 8]>,
}

// SAFETY: `tail` is an atomic and `reserved` is written once by the creator, as zero,
// and read by nobody (spec, "Metadata page").
unsafe impl Sync for TailCells {}

impl TailCells {
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            tail: AtomicI64::new(0),
            reserved: UnsafeCell::new([0; SLOT_LENGTH - 8]),
        }
    }

    /// The tail, with an acquire load.
    pub(crate) fn load(&self) -> Tail {
        Tail(self.tail.load(Ordering::Acquire))
    }

    /// Stores the tail with a release store: the exclusive publisher after a claim, and
    /// the creator at initialisation.
    pub(crate) fn store(&self, tail: Tail) {
        self.tail.store(tail.0, Ordering::Release);
    }

    /// The concurrent publisher's reservation: adds `aligned_length` and returns the
    /// tail before the add, which is the claim's term id and offset.
    pub(crate) fn fetch_add(&self, aligned_length: u32) -> Tail {
        Tail(
            self.tail
                .fetch_add(i64::from(aligned_length), Ordering::AcqRel),
        )
    }

    /// Rotation's compare-and-swap: `expected` to `next`, or the value found.
    pub(crate) fn compare_exchange(&self, expected: Tail, next: Tail) -> Result<Tail, Tail> {
        self.tail
            .compare_exchange(expected.0, next.0, Ordering::AcqRel, Ordering::Acquire)
            .map(Tail)
            .map_err(Tail)
    }
}

/// The active term count in its 128-byte slot.
#[repr(C, align(128))]
pub(crate) struct ActiveTermCountCells {
    count: AtomicI32,
    reserved: UnsafeCell<[u8; SLOT_LENGTH - 4]>,
}

// SAFETY: as `TailCells`.
unsafe impl Sync for ActiveTermCountCells {}

impl ActiveTermCountCells {
    /// The active term count, with an acquire load.
    pub(crate) fn load(&self) -> i32 {
        self.count.load(Ordering::Acquire)
    }

    /// The creator's store, before the header's magic is published: 0 for a
    /// publication's log, the start position's term count for a replay's (spec,
    /// "Metadata page").
    pub(crate) fn store(&self, count: i32) {
        self.count.store(count, Ordering::Release);
    }

    /// Rotation's compare-and-swap of the count: `expected` to `next`.
    pub(crate) fn compare_exchange(&self, expected: i32, next: i32) -> bool {
        self.count
            .compare_exchange(expected, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// The end of stream position and the is connected flag, in the slot at 640: the
/// position at its start and the flag 64 bytes on, as the spec places them.
#[repr(C, align(128))]
pub(crate) struct StreamCells {
    end_of_stream_position: AtomicI64,
    reserved_a: UnsafeCell<[u8; 56]>,
    is_connected: AtomicI32,
    reserved_b: UnsafeCell<[u8; 60]>,
}

// SAFETY: two atomics and reserved bytes written once by the creator and read by nobody.
unsafe impl Sync for StreamCells {}

impl StreamCells {
    pub(crate) fn end_of_stream_position(&self) -> i64 {
        self.end_of_stream_position.load(Ordering::Acquire)
    }

    pub(crate) fn set_end_of_stream_position(&self, position: i64) {
        self.end_of_stream_position
            .store(position, Ordering::Release);
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Acquire) == 1
    }

    pub(crate) fn set_connected(&self, connected: bool) {
        self.is_connected
            .store(i32::from(connected), Ordering::Release);
    }
}

/// The whole 4096-byte metadata page.
#[repr(C, align(128))]
pub(crate) struct MetaDataCells {
    pub(crate) header: HeaderCells,
    pub(crate) tails: [TailCells; PARTITION_COUNT],
    pub(crate) active_term_count: ActiveTermCountCells,
    pub(crate) stream: StreamCells,
    reserved: UnsafeCell<[u8; META_DATA_LENGTH - layout::IS_CONNECTED_OFFSET - 64]>,
}

// SAFETY: every field is `Sync` by the arguments above, and `reserved` is written once
// by the creator and read by nobody.
unsafe impl Sync for MetaDataCells {}

/// The first 32 bytes of a frame: the word the claim protocol stores (frame length,
/// version, flags and type), and the rest of the header as bytes. Any 32-aligned offset
/// of a term is one of these; the body that follows a header is plain bytes `file`
/// reaches by pointer, ordered by the same word.
#[repr(C)]
pub(crate) struct FrameCells {
    word: AtomicU64,
    rest: UnsafeCell<[u8; FRAME_HEADER_LENGTH - 8]>,
}

// SAFETY: `rest` is written by the publisher after its release store of the negative
// length and before its release store of the positive length (`claim`, `publish`), and
// read only after an acquire load found a positive length (`length`, `header`); a
// reader that loaded a length of 0 or less reads nothing (spec, "Claim, commit and
// abort", "Reading"). The driver rewrites a frame only when it cleans a term every
// subscriber has passed, or unblocks a claim no reader has acquired.
unsafe impl Sync for FrameCells {}

impl FrameCells {
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            word: AtomicU64::new(0),
            rest: UnsafeCell::new([0; FRAME_HEADER_LENGTH - 8]),
        }
    }

    /// Step 4 of a claim: `header` is the 32 bytes from [`layout::claim_header`], with
    /// the frame length negated. The first eight bytes go in one release store, then
    /// the other 24 as plain stores.
    pub(crate) fn claim(&self, header: &[u8; FRAME_HEADER_LENGTH]) {
        self.word
            .store(layout::frame_word(header), Ordering::Release);
        let mut rest = [0u8; FRAME_HEADER_LENGTH - 8];
        rest.copy_from_slice(&header[8..]);
        // SAFETY: the word stored above holds a negative length, so a reader that loads
        // it stops before reading `rest`; the publisher is the only writer of this
        // frame until it publishes a positive length, which orders this write before
        // the reader's acquire (`publish`). The driver's unblock rewrites an abandoned
        // claim only after the unblock timeout, a bound in time that loom cannot model.
        self.rest.with_mut(|p| unsafe { p.write(rest) });
    }

    /// Sets the reserved value of a claimed frame, header bytes 24 to 32, before commit.
    pub(crate) fn set_reserved_value(&self, value: i64) {
        // SAFETY: as `claim`: the frame is claimed and not yet published.
        self.rest.with_mut(|p| unsafe {
            let bytes = value.to_le_bytes();
            let rest = &mut *p;
            rest[16..24].copy_from_slice(&bytes);
        });
    }

    /// Step 5 of a claim, commit or abort: the committed or aborted word, with a release
    /// store that publishes the header and the body.
    pub(crate) fn publish(&self, word: u64) {
        self.word.store(word, Ordering::Release);
    }

    /// The frame word with an acquire load; its low 32 bits are the frame length, 0 for
    /// never written or cleaned, negative for a claim in progress.
    pub(crate) fn word(&self) -> u64 {
        self.word.load(Ordering::Acquire)
    }

    /// The 32 header bytes of a published frame, given the `word` an acquire load found
    /// with a positive length.
    pub(crate) fn header(&self, word: u64) -> [u8; FRAME_HEADER_LENGTH] {
        debug_assert!(layout::frame_word_length(word) > 0);
        let mut bytes = [0u8; FRAME_HEADER_LENGTH];
        bytes[..8].copy_from_slice(&word.to_le_bytes());
        // SAFETY: the caller acquired `word` with a positive length, which the publisher
        // stored with release after writing `rest` (`claim`, `publish`), and nothing
        // rewrites a published frame until the driver cleans it, which happens only
        // below every subscriber's position.
        let rest = self.rest.with(|p| unsafe { p.read() });
        bytes[8..].copy_from_slice(&rest);
        bytes
    }

    /// Cleaning, for the frame cells after the first in a range: everything to zero
    /// with plain and relaxed stores; the first cell's `clean_first` publishes them.
    pub(crate) fn clean_rest(&self) {
        // SAFETY: the range being cleaned is below every subscriber's position, so no
        // reader reads it, and the driver is the only writer of a term it cleans (spec,
        // "The driver").
        self.rest
            .with_mut(|p| unsafe { p.write([0; FRAME_HEADER_LENGTH - 8]) });
        self.word.store(0, Ordering::Relaxed);
    }

    /// Cleaning, for the first frame cell of a range: the rest to zero, then the word to
    /// zero with a release store, so a reader that loads 0 here never sees a
    /// half-cleaned range beyond it.
    pub(crate) fn clean_first(&self) {
        // SAFETY: as `clean_rest`.
        self.rest
            .with_mut(|p| unsafe { p.write([0; FRAME_HEADER_LENGTH - 8]) });
        self.word.store(0, Ordering::Release);
    }
}

// The spec's offsets and sizes, checked at compile time; under loom the atomics have
// another size and the protocols, not the layout, are under test.
#[cfg(not(loom))]
const _: () = {
    assert!(size_of::<HeaderCells>() == HEADER_LENGTH);
    assert!(size_of::<TailCells>() == SLOT_LENGTH);
    assert!(size_of::<ActiveTermCountCells>() == SLOT_LENGTH);
    assert!(size_of::<StreamCells>() == SLOT_LENGTH);
    assert!(
        core::mem::offset_of!(StreamCells, is_connected)
            == layout::IS_CONNECTED_OFFSET - layout::END_OF_STREAM_POSITION_OFFSET
    );
    assert!(size_of::<MetaDataCells>() == META_DATA_LENGTH);
    assert!(align_of::<MetaDataCells>() == SLOT_LENGTH);
    assert!(core::mem::offset_of!(MetaDataCells, tails) == layout::TAIL_COUNTERS_OFFSET);
    assert!(
        core::mem::offset_of!(MetaDataCells, active_term_count) == layout::ACTIVE_TERM_COUNT_OFFSET
    );
    assert!(core::mem::offset_of!(MetaDataCells, stream) == layout::END_OF_STREAM_POSITION_OFFSET);
    assert!(size_of::<FrameCells>() == FRAME_HEADER_LENGTH);
    assert!(align_of::<FrameCells>() == 8);
};

#[cfg(all(test, not(loom)))]
mod tests {
    use cavalry_proto::frame::DataFrameHeader;

    use super::*;
    use crate::log::layout::LogType;

    fn fields() -> HeaderFields {
        HeaderFields::new(LogType::Concurrent, 42, -5, 65536, 1408, 32768, 1, 1001).unwrap()
    }

    #[test]
    fn a_header_reads_not_ready_until_published_then_its_fields() {
        let cells = HeaderCells::zeroed();
        let length = fields().file_length();
        assert!(matches!(cells.read(length), Err(LogError::NotReady)));
        cells.publish(&fields());
        assert_eq!(cells.read(length).unwrap(), fields());
        assert!(matches!(
            cells.read(length + 1),
            Err(LogError::Length { .. })
        ));
    }

    #[test]
    fn a_frame_reads_nothing_until_committed_then_its_header() {
        let cells = FrameCells::zeroed();
        let default = fields().default_frame_header();
        let claimed = layout::claim_header(&default, 64, 40, -5, 0);
        cells.claim(&claimed);
        assert!(
            layout::frame_word_length(cells.word()) < 0,
            "a claim in progress reads negative"
        );
        cells.set_reserved_value(7);
        let committed = layout::committed_header(&layout::claim_header(&default, 64, 40, -5, 7));
        cells.publish(layout::frame_word(&committed));
        let word = cells.word();
        assert_eq!(layout::frame_word_length(word), 40);
        assert_eq!(cells.header(word), committed);
        let header = DataFrameHeader::parse_within(&cells.header(word), 4096).unwrap();
        assert_eq!(header.reserved_value(), 7);
        assert_eq!(header.term_offset(), 64);
        cells.clean_first();
        assert_eq!(cells.word(), 0);
    }

    #[test]
    fn a_tail_adds_and_rotates() {
        let cells = TailCells::zeroed();
        cells.store(Tail::pack(-5, 0));
        assert_eq!(cells.fetch_add(64), Tail::pack(-5, 0));
        assert_eq!(cells.fetch_add(32), Tail::pack(-5, 64));
        assert_eq!(cells.load(), Tail::pack(-5, 96));
        assert_eq!(
            cells.compare_exchange(Tail::pack(-5, 0), Tail::pack(-4, 0)),
            Err(Tail::pack(-5, 96))
        );
        assert_eq!(
            cells.compare_exchange(Tail::pack(-5, 96), Tail::pack(-4, 0)),
            Ok(Tail::pack(-5, 96))
        );
        assert_eq!(cells.load(), Tail::pack(-4, 0));
    }
}

#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::thread;

    use super::*;
    use crate::log::layout::LogType;

    fn fields() -> HeaderFields {
        HeaderFields::new(LogType::Concurrent, 42, -5, 65536, 1408, 32768, 1, 1001).unwrap()
    }

    /// The publisher claims (negative length, then the header's bytes), sets the
    /// reserved value and commits with a release store; a reader that acquires a
    /// positive length sees every header byte, and reads none before. loom explores
    /// every interleaving and fails on a read that races a write.
    #[test]
    fn a_reader_acquires_the_frame_the_publisher_committed() {
        loom::model(|| {
            let cells = Arc::new(FrameCells::zeroed());
            let default = fields().default_frame_header();
            let committed =
                layout::committed_header(&layout::claim_header(&default, 64, 40, -5, 7));
            let publisher = {
                let cells = Arc::clone(&cells);
                thread::spawn(move || {
                    cells.claim(&layout::claim_header(&default, 64, 40, -5, 0));
                    cells.set_reserved_value(7);
                    cells.publish(layout::frame_word(&committed));
                })
            };
            loop {
                let word = cells.word();
                if layout::frame_word_length(word) > 0 {
                    assert_eq!(cells.header(word), committed);
                    break;
                }
                thread::yield_now();
            }
            publisher.join().unwrap();
        });
    }

    /// The block scan's one step beyond `header`: after acquiring a positive length it
    /// reads the word's own eight bytes without synchronisation, as the block's slice
    /// over the mapping does (`file`, `Mapped::block`, ADR 0025), and loom checks that
    /// the read never races the publisher's two stores of the word; a scan that read
    /// them before the acquire would fail here.
    #[test]
    fn a_block_scan_reads_the_word_it_acquired_without_a_race() {
        loom::model(|| {
            let cells = Arc::new(FrameCells::zeroed());
            let default = fields().default_frame_header();
            let committed =
                layout::committed_header(&layout::claim_header(&default, 64, 40, -5, 0));
            let publisher = {
                let cells = Arc::clone(&cells);
                thread::spawn(move || {
                    cells.claim(&layout::claim_header(&default, 64, 40, -5, 0));
                    cells.publish(layout::frame_word(&committed));
                })
            };
            loop {
                let word = cells.word();
                if layout::frame_word_length(word) > 0 {
                    // SAFETY: the acquire load above saw the positive length the publisher
                    // stored with release, after which nobody writes the word; loom
                    // checks exactly that claim.
                    let plain = unsafe { cells.word.unsync_load() };
                    assert_eq!(plain, word);
                    assert_eq!(cells.header(word), committed);
                    break;
                }
                thread::yield_now();
            }
            publisher.join().unwrap();
        });
    }

    /// Two concurrent publishers reserve with fetch-and-add and get disjoint offsets.
    #[test]
    fn two_publishers_reserve_disjoint_offsets() {
        loom::model(|| {
            let cells = Arc::new(TailCells::zeroed());
            cells.store(Tail::pack(-5, 0));
            let other = {
                let cells = Arc::clone(&cells);
                thread::spawn(move || cells.fetch_add(64).raw_term_offset())
            };
            let mine = cells.fetch_add(32).raw_term_offset();
            let theirs = other.join().unwrap();
            assert!(
                (mine == 0 && theirs == 32) || (theirs == 0 && mine == 64),
                "{mine} {theirs}"
            );
            assert_eq!(cells.load(), Tail::pack(-5, 96));
        });
    }
}
