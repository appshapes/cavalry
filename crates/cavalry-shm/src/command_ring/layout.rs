//! The bytes and the arithmetic of `spec/command-ring.md`, version 1, on plain values:
//! the constants, the header encoding and its checks, the record header, and the claim,
//! commit, unblock and read arithmetic, each as a function a caller feeds the values it
//! loaded.
//! No `unsafe` and no mapping here, so Miri runs every test that uses this module and
//! the spec's vectors run against it; `cells` and `file` put the same functions over a
//! mapping.

use super::CommandRingError;
use crate::ring::{self, HeaderReject, RingHeader};

/// Length of the header at the start of the trailer, in bytes.
pub const HEADER_LENGTH: usize = ring::HEADER_LENGTH;
/// Length of the trailer: the header and five 128-byte slots.
pub const TRAILER_LENGTH: usize = 768;
/// The first eight bytes of the trailer, `CVLYCMDR`, written last by the creator.
pub const MAGIC: [u8; 8] = *b"CVLYCMDR";
/// The version of `spec/command-ring.md` this crate reads and writes: 1, since commit
/// and abort became a compare-and-swap (ADR 0018).
pub const VERSION: i32 = 1;
/// The smallest capacity a ring may have.
pub const MIN_CAPACITY: u32 = ring::MIN_CAPACITY;
/// The largest capacity a ring may have.
pub const MAX_CAPACITY: u32 = ring::MAX_CAPACITY;
/// The length of a record's header: its length and its type.
pub const RECORD_HEADER_LENGTH: u32 = ring::RECORD_HEADER_LENGTH;
/// A record begins and ends at a multiple of this.
pub const RECORD_ALIGNMENT: u32 = ring::RECORD_ALIGNMENT;
/// The type of a padding record, which the consumer skips.
pub const PADDING_TYPE: i32 = ring::PADDING_TYPE;
/// Offset of the tail position in the trailer.
pub const TAIL_POSITION_OFFSET: usize = 128;
/// Offset of the head cache position in the trailer.
pub const HEAD_CACHE_POSITION_OFFSET: usize = 256;
/// Offset of the head position in the trailer.
pub const HEAD_POSITION_OFFSET: usize = 384;
/// Offset of the correlation id counter in the trailer.
pub const CORRELATION_ID_COUNTER_OFFSET: usize = 512;
/// Offset of the consumer heartbeat in the trailer.
pub const CONSUMER_HEARTBEAT_OFFSET: usize = 640;
/// The file's name inside the driver's directory.
pub const FILE_NAME: &str = "command-ring";
/// The consumer heartbeat the driver stores when it closes.
pub const DRIVER_CLOSED: i64 = -1;

/// The header's fields after the magic, validated: the capacity is a power of two from
/// [`MIN_CAPACITY`] to [`MAX_CAPACITY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeaderFields(RingHeader);

impl HeaderFields {
    /// Fields for a ring of `capacity` bytes created by `creator_pid` at `start_timestamp`
    /// nanoseconds since the Unix epoch. Fails with [`CommandRingError::Capacity`] when
    /// the capacity is not a power of two from [`MIN_CAPACITY`] to [`MAX_CAPACITY`].
    pub fn new(
        capacity: u32,
        creator_pid: i64,
        start_timestamp: i64,
    ) -> Result<Self, CommandRingError> {
        let capacity = ring::check_capacity(i64::from(capacity)).map_err(reject)?;
        Ok(Self(RingHeader {
            capacity,
            creator_pid,
            start_timestamp,
        }))
    }

    pub(crate) const fn from_ring(header: RingHeader) -> Self {
        Self(header)
    }

    pub(crate) const fn ring(&self) -> &RingHeader {
        &self.0
    }

    /// The version this crate writes: [`VERSION`].
    pub const fn version(&self) -> i32 {
        VERSION
    }

    /// The ring's capacity in bytes.
    pub const fn capacity(&self) -> u32 {
        self.0.capacity
    }

    /// The pid of the process that created the ring.
    pub const fn creator_pid(&self) -> i64 {
        self.0.creator_pid
    }

    /// When the creator started, in nanoseconds since the Unix epoch.
    pub const fn start_timestamp(&self) -> i64 {
        self.0.start_timestamp
    }

    /// The file length these fields imply: capacity + 768.
    pub const fn file_length(&self) -> u64 {
        ring::file_length(self.0.capacity, TRAILER_LENGTH)
    }

    /// Where the trailer begins: the capacity.
    pub const fn trailer_offset(&self) -> u64 {
        self.0.capacity as u64
    }

    /// The longest message this ring carries: capacity ÷ 8.
    pub const fn max_message_length(&self) -> u32 {
        ring::max_message_length(self.0.capacity)
    }
}

/// The 128 bytes of a header for these fields, magic included.
pub fn encode_header(fields: &HeaderFields) -> [u8; HEADER_LENGTH] {
    ring::encode_header(MAGIC, VERSION, &fields.0)
}

/// What a producer does to its record's word (`spec/command-ring.md`, "Claim, commit
/// and abort", steps 7 to 9): claim it, load it before writing the message, commit, or
/// abort.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaimAction {
    /// Step 7: the swap from 0 to the claim word.
    Claim,
    /// Step 8: the acquire load before the message is written.
    Write,
    /// Step 9: the swap to the positive length and the type.
    Commit,
    /// Step 9: the swap to the positive length and type −1.
    Abort,
}

/// What a [`ClaimAction`] produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaimOutcome {
    /// The word was 0: this word is swapped in, the claim word.
    Claimed(RecordHeader),
    /// The claim is intact: the message may be written.
    Writable,
    /// The claim is intact: this word is swapped in, the committed record's header.
    Committed(RecordHeader),
    /// The claim is intact: this word is swapped in, a padding record's header.
    Aborted(RecordHeader),
    /// The word is not what the action needs, 0 for a claim and the claim word after:
    /// the driver unblocked the claim, and nothing is written. The producer reports
    /// `unblocked`.
    Unblocked,
}

/// Steps 7 to 9 on plain values: `claim` is the claim word the producer stores or
/// stored, `found` the word at the record when it acts. The cells do the same with an
/// acquire load or a compare-and-swap, where the comparison is the atomic's.
pub fn plan_claim_action(
    claim: RecordHeader,
    found: RecordHeader,
    action: ClaimAction,
) -> ClaimOutcome {
    if action == ClaimAction::Claim {
        return if found == RecordHeader::new(0, 0) {
            ClaimOutcome::Claimed(claim)
        } else {
            ClaimOutcome::Unblocked
        };
    }
    if found != claim {
        return ClaimOutcome::Unblocked;
    }
    let length = -claim.length();
    match action {
        ClaimAction::Claim => ClaimOutcome::Claimed(claim),
        ClaimAction::Write => ClaimOutcome::Writable,
        ClaimAction::Commit => ClaimOutcome::Committed(RecordHeader::new(length, claim.type_id())),
        ClaimAction::Abort => ClaimOutcome::Aborted(RecordHeader::new(length, PADDING_TYPE)),
    }
}

/// Reads a header from the front of `bytes` for a file of `file_length` bytes and runs
/// the spec's checks in order: too short, not ready, magic, version, capacity, length.
/// The `changed` check needs two reads and lives in the file layer.
pub fn parse_header(bytes: &[u8], file_length: u64) -> Result<HeaderFields, CommandRingError> {
    ring::parse_header(bytes, file_length, MAGIC, VERSION, TRAILER_LENGTH)
        .map(HeaderFields)
        .map_err(reject)
}

/// A shared header rejection as this layout's error.
pub(crate) fn reject(reject: HeaderReject) -> CommandRingError {
    match reject {
        HeaderReject::TooShort(given) => CommandRingError::TooShort { given },
        HeaderReject::NotReady => CommandRingError::NotReady,
        HeaderReject::Magic(found) => CommandRingError::Magic { found },
        HeaderReject::Version(found) => CommandRingError::Version { found },
        HeaderReject::Capacity(found) => CommandRingError::Capacity { found },
        HeaderReject::Length { found, needed } => CommandRingError::Length { found, needed },
    }
}

/// A length rounded up to the record alignment.
pub const fn align_record(length: u32) -> u32 {
    ring::align_record(length)
}

/// The longest message a ring of `capacity` carries: capacity ÷ 8.
pub const fn max_message_length(capacity: u32) -> u32 {
    ring::max_message_length(capacity)
}

// not a state machine: what `RecordHeader::state` reads in one header. The word's moves
// are the ring's claim protocol, once per command; the tail and head positions measure
// them, the driver's moves are `command_ring.unblocked` and `command_ring.rejected_record`,
// and a producer's abort is a client's, which owns no counter (spec/command-ring.md).
/// What a record's header says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecordState {
    /// Length 0: nothing claimed here yet, or cleared. A reader stops.
    Unclaimed,
    /// A negative length: a claim in progress. A reader stops; the unblocker acts.
    Claimed,
    /// A positive length and a type of 1 or more: a message.
    Committed,
    /// A positive length and type −1: skipped.
    Padding,
}

/// A record's header: the length and the type a reader loads in one 8-byte word
/// (`spec/command-ring.md`, "Record").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RecordHeader {
    length: i32,
    type_id: i32,
}

impl RecordHeader {
    /// A header of `length` and `type_id`, unchecked: [`RecordHeader::check`] applies the
    /// spec's bounds.
    pub const fn new(length: i32, type_id: i32) -> Self {
        Self { length, type_id }
    }

    /// The length field: 0, or 8 + the message length, negative while claimed.
    pub const fn length(self) -> i32 {
        self.length
    }

    /// The type field: −1 for padding, 1 or more for a message.
    pub const fn type_id(self) -> i32 {
        self.type_id
    }

    /// The header as the word the ring stores: the length low, the type high.
    pub const fn word(self) -> u64 {
        ring::record_word(self.length, self.type_id)
    }

    /// The header a ring word holds.
    pub const fn from_word(word: u64) -> Self {
        Self {
            length: ring::word_length(word),
            type_id: ring::word_type(word),
        }
    }

    /// The header's eight bytes, little-endian.
    pub const fn encode(self) -> [u8; 8] {
        self.word().to_le_bytes()
    }

    /// The header in eight little-endian bytes.
    pub const fn decode(bytes: &[u8; 8]) -> Self {
        Self::from_word(u64::from_le_bytes(*bytes))
    }

    /// What the header says the record is.
    pub const fn state(self) -> RecordState {
        if self.length == 0 {
            RecordState::Unclaimed
        } else if self.length < 0 {
            RecordState::Claimed
        } else if self.type_id == PADDING_TYPE {
            RecordState::Padding
        } else {
            RecordState::Committed
        }
    }

    /// Whether the type is padding.
    pub const fn is_padding(self) -> bool {
        self.type_id == PADDING_TYPE
    }

    /// The length's magnitude rounded up to the record alignment; 0 when unclaimed.
    pub const fn aligned_length(self) -> u32 {
        align_record(self.length.unsigned_abs())
    }

    /// The message's length: the length's magnitude less the header, 0 when there is
    /// no message.
    pub const fn message_length(self) -> u32 {
        self.length
            .unsigned_abs()
            .saturating_sub(RECORD_HEADER_LENGTH)
    }

    /// The spec's bounds on a record at `index` in a ring of `capacity`: an unclaimed
    /// record passes; otherwise the magnitude is 8 or more and its aligned length fits
    /// before the end of the ring, the type is −1 or 1 or more, and a message is at most
    /// capacity ÷ 8. [`CommandRingError::Record`] names the failure.
    pub fn check(self, capacity: u32, index: u32) -> Result<(), CommandRingError> {
        if self.length == 0 {
            return Ok(());
        }
        let magnitude = self.length.unsigned_abs();
        let remaining = capacity.saturating_sub(index);
        let fits = magnitude >= RECORD_HEADER_LENGTH && align_record(magnitude) <= remaining;
        let typed = self.type_id == PADDING_TYPE
            || (self.type_id >= 1
                && magnitude.saturating_sub(RECORD_HEADER_LENGTH) <= max_message_length(capacity));
        if fits && typed {
            Ok(())
        } else {
            Err(CommandRingError::Record {
                index,
                length: self.length,
                type_id: self.type_id,
            })
        }
    }
}

/// The consumer's `tail` check, against the head it owns: both multiples of 8, the head
/// 0 or more, the tail from the head to the head + capacity. Sound for the consumer,
/// whose head is exact and whose tail load is at least the tail of every record it has
/// read; not for a producer, whose head load may be fresher than its tail load.
pub fn check_tail(capacity: u32, tail: i64, head: i64) -> Result<(), CommandRingError> {
    let aligned =
        tail % i64::from(RECORD_ALIGNMENT) == 0 && head % i64::from(RECORD_ALIGNMENT) == 0;
    if aligned && head >= 0 && tail >= head && tail - head <= i64::from(capacity) {
        Ok(())
    } else {
        Err(CommandRingError::Tail { tail, head })
    }
}

/// The `tail` check a producer runs against the head cache, before it knows the head:
/// both multiples of 8, the cache 0 or more, the tail at least the cache.
pub fn check_tail_against_cache(tail: i64, head_cache: i64) -> Result<(), CommandRingError> {
    let aligned =
        tail % i64::from(RECORD_ALIGNMENT) == 0 && head_cache % i64::from(RECORD_ALIGNMENT) == 0;
    if aligned && head_cache >= 0 && tail >= head_cache {
        Ok(())
    } else {
        Err(CommandRingError::Tail {
            tail,
            head: head_cache,
        })
    }
}

/// A padding record a claim or an unblock writes: where, and how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaddingRecord {
    /// The record's index in the ring.
    pub index: u32,
    /// The record's length, a multiple of 8.
    pub length: u32,
}

/// Whether a claim found room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaimResult {
    /// The record is claimed at this index once the tail is swapped.
    Claimed {
        /// The record's index in the ring.
        index: u32,
    },
    /// Fewer free bytes than the claim needs: nothing but the padding, if any, is
    /// written.
    Full,
}

/// What one pass of the claim protocol decided from the values a producer loaded
/// (`spec/command-ring.md`, "Claim, commit and abort", steps 2 to 4): the producer
/// stores the head cache if asked, compare-and-swaps the tail to `tail_after`, writes
/// the padding record if any, and then the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClaimPlan {
    /// Claimed at an index, or full.
    pub result: ClaimResult,
    /// 8 + the message length.
    pub record_length: u32,
    /// The record length rounded up to the alignment: what the tail advances by.
    pub aligned_length: u32,
    /// The tail to swap in: the tail loaded + the aligned length + the padding, or the
    /// tail + the padding alone when full.
    pub tail_after: i64,
    /// The head the producer loaded and stores in the head cache, when it loaded one.
    pub head_cache_after: Option<i64>,
    /// The padding record to the end of the ring, when the record did not fit there.
    pub padding: Option<PaddingRecord>,
}

/// One pass of the claim arithmetic for a message of `message_length` bytes, from the
/// `head_cache` and then the `tail` the producer loaded; `load_head` is called when the
/// cache says the ring may be full, at most once. The head it returns may be beyond a
/// stale tail: the plan then asks for a compare-and-swap that fails, and the producer
/// loads again. [`CommandRingError::Message`] for a message over capacity ÷ 8,
/// [`CommandRingError::Tail`] for a tail that is not a multiple of 8 or is below the
/// head cache; only the consumer, which owns the head, checks the tail against it.
pub fn plan_claim(
    capacity: u32,
    tail: i64,
    head_cache: i64,
    message_length: usize,
    mut load_head: impl FnMut() -> i64,
) -> Result<ClaimPlan, CommandRingError> {
    let limit = max_message_length(capacity) as usize;
    if message_length > limit {
        return Err(CommandRingError::Message {
            found: message_length,
            limit,
        });
    }
    let record_length = RECORD_HEADER_LENGTH + message_length as u32;
    let required = align_record(record_length);
    check_tail_against_cache(tail, head_cache)?;
    let cap = i64::from(capacity);
    let mask = i64::from(capacity - 1);
    let mut head = head_cache;
    let mut head_cache_after = None;
    let mut reread = false;
    if i64::from(required) > cap - (tail - head) {
        head = load_head();
        reread = true;
        if i64::from(required) > cap - (tail - head) {
            return Ok(ClaimPlan {
                result: ClaimResult::Full,
                record_length,
                aligned_length: required,
                tail_after: tail,
                head_cache_after: None,
                padding: None,
            });
        }
        head_cache_after = Some(head);
    }
    let mut tail_after = tail + i64::from(required);
    let tail_index = (tail & mask) as u32;
    let to_end = capacity - tail_index;
    let mut result = ClaimResult::Claimed { index: tail_index };
    let mut padding = None;
    if required > to_end {
        result = ClaimResult::Claimed { index: 0 };
        let mut head_index = (head & mask) as u32;
        if required > head_index {
            if !reread {
                head = load_head();
            }
            head_index = (head & mask) as u32;
            if required > head_index {
                result = ClaimResult::Full;
                tail_after = tail;
            }
            head_cache_after = Some(head);
        }
        padding = Some(PaddingRecord {
            index: tail_index,
            length: to_end,
        });
        tail_after += i64::from(to_end);
    }
    Ok(ClaimPlan {
        result,
        record_length,
        aligned_length: required,
        tail_after,
        head_cache_after,
        padding,
    })
}

/// The unblock arithmetic (`spec/command-ring.md`, "Unblocking") from the `head` and
/// `tail` the driver loaded, with `load_word` the acquire load of the record word at an
/// index: the padding record to write, or `None` when nothing is blocked or when a gap
/// of zeros ends at the tail's index, which the next claim will bound.
/// [`CommandRingError::Tail`] for a tail outside its bound,
/// [`CommandRingError::Record`] for a stale claim outside the record bounds.
pub fn plan_unblock(
    capacity: u32,
    head: i64,
    tail: i64,
    mut load_word: impl FnMut(u32) -> u64,
) -> Result<Option<PaddingRecord>, CommandRingError> {
    check_tail(capacity, tail, head)?;
    if head == tail {
        return Ok(None);
    }
    let mask = i64::from(capacity - 1);
    let consumer = (head & mask) as u32;
    let producer = (tail & mask) as u32;
    let header = RecordHeader::from_word(load_word(consumer));
    if header.length() < 0 {
        header.check(capacity, consumer)?;
        return Ok(Some(PaddingRecord {
            index: consumer,
            length: header.length().unsigned_abs(),
        }));
    }
    if header.length() > 0 {
        return Ok(None);
    }
    let limit = if producer > consumer {
        producer
    } else {
        capacity
    };
    let mut index = consumer + RECORD_ALIGNMENT;
    while index < limit && ring::word_length(load_word(index)) == 0 {
        index += RECORD_ALIGNMENT;
    }
    if index == limit && limit == producer {
        // Only zeros up to the tail's index: the next claim bounds the gap, and padding
        // it now would cover a live client's fresh claim. Nothing is written.
        return Ok(None);
    }
    // A nonzero word at `index`, or the end of the ring reached with only zeros, which
    // no later claim can bound. A claim may have landed behind the scan: confirm every
    // word back to the consumer index, that one included, is still 0 before covering
    // the gap.
    let mut back = index;
    while back > consumer {
        back -= RECORD_ALIGNMENT;
        if ring::word_length(load_word(back)) != 0 {
            return Ok(None);
        }
    }
    Ok(Some(PaddingRecord {
        index: consumer,
        length: index - consumer,
    }))
}

/// Record words by index, for the read arithmetic: a byte slice for the vectors and
/// Miri, the ring's atomics with acquire loads in production.
pub trait RecordWords {
    /// The 8-byte record word at byte `index`, little-endian.
    fn record_word(&self, index: u32) -> u64;
}

impl RecordWords for [u8] {
    fn record_word(&self, index: u32) -> u64 {
        ring::word_at(self, index)
    }
}

/// Why a read stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadStop {
    /// The bytes read reached the end of the ring; the next read begins at index 0.
    EndOfRing,
    /// The next record is unclaimed: nothing more is committed.
    Unclaimed,
    /// The next record is a claim in progress: nothing more is committed.
    Claimed,
    /// As many messages as the caller asked for were delivered.
    Limit,
    /// The next record is outside the spec's bounds: the ring is corrupt from there.
    Record,
}

/// What one read did (`spec/command-ring.md`, "Reading"): the caller zeroes
/// `bytes_read` from the head's index and releases the head + `bytes_read`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReadPlan {
    /// Why the read stopped.
    pub stop: ReadStop,
    /// How many bytes the read consumed, padding included.
    pub bytes_read: u32,
    /// How many messages were delivered.
    pub messages: usize,
    /// The record that stopped the read with [`ReadStop::Record`], and its index.
    pub rejected: Option<(u32, RecordHeader)>,
}

/// The read arithmetic from the consumer's `head`, delivering up to `limit` messages to
/// `on_record` as the index and header of each committed record, in order, with the
/// bytes from the head's index to the end of the ring as the most one read covers.
pub fn plan_read<W: RecordWords + ?Sized>(
    capacity: u32,
    head: i64,
    limit: usize,
    words: &W,
    mut on_record: impl FnMut(u32, RecordHeader),
) -> ReadPlan {
    let head_index = (head as u64 & u64::from(capacity - 1)) as u32;
    let max_block = capacity - head_index;
    let mut bytes_read = 0;
    let mut messages = 0;
    let mut rejected = None;
    let stop = loop {
        if bytes_read >= max_block {
            break ReadStop::EndOfRing;
        }
        if messages >= limit {
            break ReadStop::Limit;
        }
        let index = head_index + bytes_read;
        let header = RecordHeader::from_word(words.record_word(index));
        match header.state() {
            RecordState::Unclaimed => break ReadStop::Unclaimed,
            RecordState::Claimed => break ReadStop::Claimed,
            RecordState::Committed | RecordState::Padding => {}
        }
        if header.check(capacity, index).is_err() {
            rejected = Some((index, header));
            break ReadStop::Record;
        }
        bytes_read += header.aligned_length();
        if header.is_padding() {
            continue;
        }
        on_record(index, header);
        messages += 1;
    };
    ReadPlan {
        stop,
        bytes_read,
        messages,
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_header_round_trips_and_names_its_state() {
        let committed = RecordHeader::new(24, 1);
        assert_eq!(RecordHeader::decode(&committed.encode()), committed);
        assert_eq!(committed.state(), RecordState::Committed);
        assert_eq!(committed.aligned_length(), 24);
        assert_eq!(committed.message_length(), 16);
        assert_eq!(RecordHeader::new(-21, 7).state(), RecordState::Claimed);
        assert_eq!(RecordHeader::new(-21, 7).aligned_length(), 24);
        assert_eq!(RecordHeader::new(8, -1).state(), RecordState::Padding);
        assert_eq!(RecordHeader::new(0, 0).state(), RecordState::Unclaimed);
        assert_eq!(RecordHeader::new(0, 0).message_length(), 0);
    }

    #[test]
    fn the_bounds_refuse_what_no_client_writes() {
        assert!(RecordHeader::new(0, 0).check(4096, 0).is_ok());
        assert!(RecordHeader::new(4, 1).check(4096, 0).is_err());
        assert!(RecordHeader::new(16, 1).check(4096, 4088).is_err());
        assert!(RecordHeader::new(8, -1).check(4096, 4088).is_ok());
        assert!(RecordHeader::new(24, 0).check(4096, 0).is_err());
        assert!(RecordHeader::new(24, -2).check(4096, 0).is_err());
        assert!(RecordHeader::new(521, 2).check(4096, 0).is_err());
        assert!(RecordHeader::new(520, 2).check(4096, 0).is_ok());
    }

    #[test]
    fn a_claim_fits_or_wraps_or_is_full() {
        let plan = plan_claim(4096, 0, 0, 16, || 0).unwrap();
        assert_eq!(plan.result, ClaimResult::Claimed { index: 0 });
        assert_eq!((plan.tail_after, plan.padding), (24, None));

        let plan = plan_claim(4096, 4088, 2048, 16, || unreachable!()).unwrap();
        assert_eq!(plan.result, ClaimResult::Claimed { index: 0 });
        assert_eq!(plan.tail_after, 4120);
        assert_eq!(
            plan.padding,
            Some(PaddingRecord {
                index: 4088,
                length: 8
            })
        );

        let plan = plan_claim(4096, 4096, 0, 16, || 0).unwrap();
        assert_eq!(plan.result, ClaimResult::Full);
        assert_eq!(plan.tail_after, 4096);

        assert_eq!(
            plan_claim(4096, 0, 8, 16, || 0).unwrap_err().reason(),
            "tail"
        );
        assert_eq!(
            plan_claim(4096, 0, 0, 513, || 0).unwrap_err().reason(),
            "message"
        );
    }

    #[test]
    fn an_unblock_pads_a_stale_claim_or_a_gap() {
        let words = |lengths: &'static [i32]| {
            move |index: u32| {
                let step = (index / 8) as usize;
                let length = lengths.get(step).copied().unwrap_or(0);
                ring::record_word(length, if length == 0 { 0 } else { 1 })
            }
        };
        assert_eq!(plan_unblock(4096, 0, 0, words(&[])).unwrap(), None);
        assert_eq!(
            plan_unblock(4096, 0, 24, words(&[-24])).unwrap(),
            Some(PaddingRecord {
                index: 0,
                length: 24
            })
        );
        assert_eq!(
            plan_unblock(4096, 0, 48, words(&[0, 0, 0, -24])).unwrap(),
            Some(PaddingRecord {
                index: 0,
                length: 24
            })
        );
        assert_eq!(plan_unblock(4096, 0, 24, words(&[24])).unwrap(), None);
        assert_eq!(
            plan_unblock(4096, 0, 24, words(&[0, 0, 0])).unwrap(),
            None,
            "a gap to the tail's index waits for the next claim"
        );
        assert_eq!(
            plan_unblock(4096, 4080, 4120, words(&[0, 0])).unwrap(),
            Some(PaddingRecord {
                index: 4080,
                length: 16
            }),
            "a gap to the end of the ring is covered"
        );
        assert_eq!(
            plan_unblock(4096, 24, 0, words(&[])).unwrap_err().reason(),
            "tail"
        );
    }

    #[test]
    fn a_read_delivers_committed_records_and_stops_by_name() {
        let mut bytes = vec![0u8; 4096];
        bytes[..8].copy_from_slice(&RecordHeader::new(24, 1).encode());
        bytes[24..32].copy_from_slice(&RecordHeader::new(16, -1).encode());
        bytes[40..48].copy_from_slice(&RecordHeader::new(-24, 2).encode());
        let mut seen = Vec::new();
        let plan = plan_read(4096, 0, 16, &bytes[..], |index, header| {
            seen.push((index, header.type_id()))
        });
        assert_eq!(plan.stop, ReadStop::Claimed);
        assert_eq!((plan.bytes_read, plan.messages), (40, 1));
        assert_eq!(seen, vec![(0, 1)]);

        bytes[40..48].copy_from_slice(&RecordHeader::new(24, 0).encode());
        let plan = plan_read(4096, 0, 16, &bytes[..], |_, _| {});
        assert_eq!(plan.stop, ReadStop::Record);
        assert_eq!(plan.rejected, Some((40, RecordHeader::new(24, 0))));
    }
}
