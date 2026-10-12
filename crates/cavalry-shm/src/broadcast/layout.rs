//! The bytes and the arithmetic of `spec/broadcast-buffer.md`, version 0, on plain
//! values: the constants, the header encoding and its checks, the record header, the
//! trailer's bounds, and the transmit and receive arithmetic, each as a function a caller
//! feeds the values it loaded. No `unsafe` and no mapping here, so Miri runs every test
//! that uses this module and the spec's vectors run against it; `cells` and `file` put
//! the same functions over a mapping.

use super::BroadcastError;
use crate::ring::{self, HeaderReject, RingHeader};

/// Length of the header at the start of the trailer, in bytes.
pub const HEADER_LENGTH: usize = ring::HEADER_LENGTH;
/// Length of the trailer: the header and three 128-byte slots.
pub const TRAILER_LENGTH: usize = 512;
/// The first eight bytes of the trailer, `CVLYBCST`, written last by the creator.
pub const MAGIC: [u8; 8] = *b"CVLYBCST";
/// The version of `spec/broadcast-buffer.md` this crate reads and writes.
pub const VERSION: i32 = 0;
/// The smallest capacity a buffer may have.
pub const MIN_CAPACITY: u32 = ring::MIN_CAPACITY;
/// The largest capacity a buffer may have.
pub const MAX_CAPACITY: u32 = ring::MAX_CAPACITY;
/// The length of a record's header: its length and its type.
pub const RECORD_HEADER_LENGTH: u32 = ring::RECORD_HEADER_LENGTH;
/// A record begins and ends at a multiple of this.
pub const RECORD_ALIGNMENT: u32 = ring::RECORD_ALIGNMENT;
/// The type of a padding record, which a receiver skips.
pub const PADDING_TYPE: i32 = ring::PADDING_TYPE;
/// Offset of the tail intent in the trailer.
pub const TAIL_INTENT_OFFSET: usize = 128;
/// Offset of the tail position in the trailer.
pub const TAIL_POSITION_OFFSET: usize = 256;
/// Offset of the latest record in the trailer.
pub const LATEST_RECORD_OFFSET: usize = 384;
/// The file's name inside the driver's directory.
pub const FILE_NAME: &str = "broadcast-buffer";

/// The header's fields after the magic, validated: the capacity is a power of two from
/// [`MIN_CAPACITY`] to [`MAX_CAPACITY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeaderFields(RingHeader);

impl HeaderFields {
    /// Fields for a buffer of `capacity` bytes created by `creator_pid` at
    /// `start_timestamp` nanoseconds since the Unix epoch. Fails with
    /// [`BroadcastError::Capacity`] when the capacity is not a power of two from
    /// [`MIN_CAPACITY`] to [`MAX_CAPACITY`].
    pub fn new(
        capacity: u32,
        creator_pid: i64,
        start_timestamp: i64,
    ) -> Result<Self, BroadcastError> {
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

    /// The buffer's capacity in bytes.
    pub const fn capacity(&self) -> u32 {
        self.0.capacity
    }

    /// The pid of the process that created the buffer.
    pub const fn creator_pid(&self) -> i64 {
        self.0.creator_pid
    }

    /// When the creator started, in nanoseconds since the Unix epoch.
    pub const fn start_timestamp(&self) -> i64 {
        self.0.start_timestamp
    }

    /// The file length these fields imply: capacity + 512.
    pub const fn file_length(&self) -> u64 {
        ring::file_length(self.0.capacity, TRAILER_LENGTH)
    }

    /// Where the trailer begins: the capacity.
    pub const fn trailer_offset(&self) -> u64 {
        self.0.capacity as u64
    }

    /// The longest message this buffer carries: capacity ÷ 8.
    pub const fn max_message_length(&self) -> u32 {
        ring::max_message_length(self.0.capacity)
    }
}

/// The 128 bytes of a header for these fields, magic included.
pub fn encode_header(fields: &HeaderFields) -> [u8; HEADER_LENGTH] {
    ring::encode_header(MAGIC, VERSION, &fields.0)
}

/// Reads a header from the front of `bytes` for a file of `file_length` bytes and runs
/// the spec's checks in order: too short, not ready, magic, version, capacity, length.
/// The `changed` check needs two reads and lives in the file layer.
pub fn parse_header(bytes: &[u8], file_length: u64) -> Result<HeaderFields, BroadcastError> {
    ring::parse_header(bytes, file_length, MAGIC, VERSION, TRAILER_LENGTH)
        .map(HeaderFields)
        .map_err(reject)
}

/// A shared header rejection as this layout's error.
pub(crate) fn reject(reject: HeaderReject) -> BroadcastError {
    match reject {
        HeaderReject::TooShort(given) => BroadcastError::TooShort { given },
        HeaderReject::NotReady => BroadcastError::NotReady,
        HeaderReject::Magic(found) => BroadcastError::Magic { found },
        HeaderReject::Version(found) => BroadcastError::Version { found },
        HeaderReject::Capacity(found) => BroadcastError::Capacity { found },
        HeaderReject::Length { found, needed } => BroadcastError::Length { found, needed },
    }
}

/// A length rounded up to the record alignment.
pub const fn align_record(length: u32) -> u32 {
    ring::align_record(length)
}

/// The longest message a buffer of `capacity` carries: capacity ÷ 8.
pub const fn max_message_length(capacity: u32) -> u32 {
    ring::max_message_length(capacity)
}

/// What a record's header says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecordState {
    /// A type of 1 or more: an event.
    Message,
    /// Type −1: the end of the ring, skipped.
    Padding,
}

/// A record's header: the length and the type a receiver loads in one 8-byte word
/// (`spec/broadcast-buffer.md`, "Record").
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

    /// The length field: 8 + the message length.
    pub const fn length(self) -> i32 {
        self.length
    }

    /// The type field: −1 for padding, 1 or more for an event.
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

    /// What the header says the record is, once it passed [`RecordHeader::check`].
    pub const fn state(self) -> RecordState {
        if self.type_id == PADDING_TYPE {
            RecordState::Padding
        } else {
            RecordState::Message
        }
    }

    /// Whether the type is padding.
    pub const fn is_padding(self) -> bool {
        self.type_id == PADDING_TYPE
    }

    /// The length rounded up to the record alignment.
    pub const fn aligned_length(self) -> u32 {
        align_record(self.length.unsigned_abs())
    }

    /// The message's length: the length less the header, 0 when there is no message.
    pub const fn message_length(self) -> u32 {
        self.length
            .unsigned_abs()
            .saturating_sub(RECORD_HEADER_LENGTH)
    }

    /// The spec's bounds on a record at `index` in a ring of `capacity`: the length is 8
    /// or more and its aligned length fits before the end of the ring; a padding record
    /// reaches the end exactly; a message's type is 1 or more and its length at most
    /// capacity ÷ 8. [`BroadcastError::Record`] names the failure.
    pub fn check(self, capacity: u32, index: u32) -> Result<(), BroadcastError> {
        let remaining = capacity.saturating_sub(index);
        let fits = self.length >= RECORD_HEADER_LENGTH as i32
            && align_record(self.length as u32) <= remaining;
        let typed = if self.type_id == PADDING_TYPE {
            self.length as u32 == remaining
        } else {
            self.type_id >= 1
                && (self.length as u32).saturating_sub(RECORD_HEADER_LENGTH)
                    <= max_message_length(capacity)
        };
        if fits && typed {
            Ok(())
        } else {
            Err(BroadcastError::Record {
                index,
                length: self.length,
                type_id: self.type_id,
            })
        }
    }
}

/// The three positions a receiver loads from the trailer, with their bounds
/// (`spec/broadcast-buffer.md`, "Trailer").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Positions {
    /// Where the next record begins.
    pub tail: i64,
    /// Where the tail will be once the record being written is complete.
    pub tail_intent: i64,
    /// Where the last complete record begins.
    pub latest: i64,
}

impl Positions {
    /// The `tail` check on positions loaded tail first, then tail intent, then latest
    /// record: every one a multiple of 8, the tail 0 or more, the tail intent at least
    /// the tail, the latest record 0 or more and at least the tail − capacity. These are
    /// the bounds that hold for any three values loaded in that order: the transmitter
    /// stores the tail intent before a record and the latest record before the tail, so
    /// the two loaded after the tail are at least the ones stored before it, and may be
    /// later ones. `next_record` goes in the error for the message.
    pub fn check(self, capacity: u32, next_record: i64) -> Result<(), BroadcastError> {
        let step = i64::from(RECORD_ALIGNMENT);
        let aligned =
            self.tail % step == 0 && self.tail_intent % step == 0 && self.latest % step == 0;
        let tail = self.tail >= 0;
        let intent = self.tail_intent >= self.tail;
        let latest = self.latest >= 0 && self.latest >= self.tail - i64::from(capacity);
        if aligned && tail && intent && latest {
            Ok(())
        } else {
            Err(BroadcastError::Tail {
                tail: self.tail,
                tail_intent: self.tail_intent,
                latest: self.latest,
                next_record,
            })
        }
    }
}

/// Whether a receiver at `cursor` has been lapped: its cursor + capacity is not above
/// the `tail_intent`, so the bytes at the cursor are being or have been overwritten.
pub const fn is_lapped(cursor: i64, capacity: u32, tail_intent: i64) -> bool {
    cursor + capacity as i64 <= tail_intent
}

/// A padding record a transmit writes to the end of the ring: where, and how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaddingRecord {
    /// The record's index in the ring.
    pub index: u32,
    /// The record's length: the bytes to the end of the ring.
    pub length: u32,
}

/// What one transmit does (`spec/broadcast-buffer.md`, "Transmitting"): the transmitter
/// stores `tail_intent`, writes the padding if any, writes the record at `index`, then
/// stores `latest` and `tail_after`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TransmitPlan {
    /// 8 + the message length.
    pub record_length: u32,
    /// The record length rounded up to the alignment.
    pub aligned_length: u32,
    /// The tail intent to store first: where the tail will be when the record is complete.
    pub tail_intent: i64,
    /// The padding record to the end of the ring, when the record did not fit there.
    pub padding: Option<PaddingRecord>,
    /// The record's index in the ring.
    pub index: u32,
    /// The record's position: the latest record once it is complete.
    pub latest: i64,
    /// The tail after the record.
    pub tail_after: i64,
}

/// The transmit arithmetic for a message of `message_length` bytes from the transmitter's
/// own `tail`. [`BroadcastError::Message`] for a message over capacity ÷ 8.
pub fn plan_transmit(
    capacity: u32,
    tail: i64,
    message_length: usize,
) -> Result<TransmitPlan, BroadcastError> {
    let limit = max_message_length(capacity) as usize;
    if message_length > limit {
        return Err(BroadcastError::Message {
            found: message_length,
            limit,
        });
    }
    let record_length = RECORD_HEADER_LENGTH + message_length as u32;
    let aligned_length = align_record(record_length);
    let mask = i64::from(capacity - 1);
    let mut index = (tail & mask) as u32;
    let to_end = capacity - index;
    let mut latest = tail;
    let (tail_intent, padding) = if to_end < aligned_length {
        latest += i64::from(to_end);
        let padding = PaddingRecord {
            index,
            length: to_end,
        };
        index = 0;
        (
            tail + i64::from(aligned_length) + i64::from(to_end),
            Some(padding),
        )
    } else {
        (tail + i64::from(aligned_length), None)
    };
    Ok(TransmitPlan {
        record_length,
        aligned_length,
        tail_intent,
        padding,
        index,
        latest,
        tail_after: latest + i64::from(aligned_length),
    })
}

/// What one receive found, before the copy and the validation that follow
/// (`spec/broadcast-buffer.md`, "Receiving", steps 1 to 3). A tail that is not above
/// the receiver's next record is [`ReceivePlan::None`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReceivePlan {
    /// The tail is at the receiver's next record: nothing is available.
    None,
    /// The receiver's cursor was a ring or more behind the tail intent: it repositions
    /// to the latest record and reports `lapped`.
    Lapped,
    /// A record to copy out from `index`, then validate against the tail intent again.
    Record {
        /// The record's position.
        cursor: i64,
        /// The record's index in the ring.
        index: u32,
        /// The position after the record.
        next_record: i64,
        /// The record's header.
        header: RecordHeader,
    },
}

/// The receive arithmetic from the receiver's `next_record` and the `positions` it
/// loaded, with `load_header` the load of the record word at an index and
/// `load_tail_intent` the acquire load, after an acquire fence, the receiver runs when
/// a record fails its bounds, to tell a lap from a corrupt record.
/// [`BroadcastError::Tail`] for positions outside their bounds,
/// [`BroadcastError::Record`] for a record outside its bounds when not lapped.
pub fn plan_receive(
    capacity: u32,
    next_record: i64,
    positions: Positions,
    mut load_header: impl FnMut(u32) -> u64,
    mut load_tail_intent: impl FnMut() -> i64,
) -> Result<ReceivePlan, BroadcastError> {
    positions.check(capacity, next_record)?;
    if positions.tail <= next_record {
        // A tail below the next record is a stale load after a lap repositioned the
        // receiver at a fresher latest record; the next receive catches up.
        return Ok(ReceivePlan::None);
    }
    let mut cursor = next_record;
    if is_lapped(cursor, capacity, positions.tail_intent) {
        return Ok(ReceivePlan::Lapped);
    }
    let mask = i64::from(capacity - 1);
    let mut index = (cursor & mask) as u32;
    let mut header = RecordHeader::from_word(load_header(index));
    if let Err(error) = header.check(capacity, index) {
        return if is_lapped(cursor, capacity, load_tail_intent()) {
            Ok(ReceivePlan::Lapped)
        } else {
            Err(error)
        };
    }
    let mut following = cursor + i64::from(header.aligned_length());
    if header.is_padding() {
        cursor = following;
        index = 0;
        header = RecordHeader::from_word(load_header(index));
        if let Err(error) = header.check(capacity, index) {
            return if is_lapped(cursor, capacity, load_tail_intent()) {
                Ok(ReceivePlan::Lapped)
            } else {
                Err(error)
            };
        }
        following = cursor + i64::from(header.aligned_length());
    }
    Ok(ReceivePlan::Record {
        cursor,
        index,
        next_record: following,
        header,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_header_round_trips_and_is_bounded() {
        let message = RecordHeader::new(24, 1);
        assert_eq!(RecordHeader::decode(&message.encode()), message);
        assert_eq!(message.state(), RecordState::Message);
        assert_eq!(
            (message.aligned_length(), message.message_length()),
            (24, 16)
        );
        assert!(message.check(4096, 0).is_ok());
        assert!(RecordHeader::new(8, -1).check(4096, 4088).is_ok());
        assert!(
            RecordHeader::new(16, -1).check(4096, 2048).is_err(),
            "padding reaches the end"
        );
        assert!(
            RecordHeader::new(0, 0).check(4096, 0).is_err(),
            "zero is not a record here"
        );
        assert!(RecordHeader::new(24, 0).check(4096, 0).is_err());
        assert!(RecordHeader::new(521, 2).check(4096, 0).is_err());
        assert!(RecordHeader::new(16, 1).check(4096, 4088).is_err());
    }

    #[test]
    fn a_transmit_fits_or_pads_to_the_end() {
        let plan = plan_transmit(4096, 0, 16).unwrap();
        assert_eq!(
            (plan.tail_intent, plan.index, plan.latest, plan.tail_after),
            (24, 0, 0, 24)
        );
        assert_eq!(plan.padding, None);
        let plan = plan_transmit(4096, 4088, 16).unwrap();
        assert_eq!(
            (plan.tail_intent, plan.index, plan.latest, plan.tail_after),
            (4120, 0, 4096, 4120)
        );
        assert_eq!(
            plan.padding,
            Some(PaddingRecord {
                index: 4088,
                length: 8
            })
        );
        assert_eq!(plan_transmit(4096, 0, 513).unwrap_err().reason(), "message");
    }

    #[test]
    fn a_receive_finds_a_record_a_lap_or_nothing() {
        let positions = |tail, tail_intent, latest| Positions {
            tail,
            tail_intent,
            latest,
        };
        let header =
            |length: i32, type_id: i32| move |_: u32| RecordHeader::new(length, type_id).word();
        assert_eq!(
            plan_receive(4096, 24, positions(24, 24, 0), header(24, 1), || 24).unwrap(),
            ReceivePlan::None
        );
        assert_eq!(
            plan_receive(4096, 0, positions(24, 24, 0), header(24, 1), || 24).unwrap(),
            ReceivePlan::Record {
                cursor: 0,
                index: 0,
                next_record: 24,
                header: RecordHeader::new(24, 1)
            }
        );
        assert_eq!(
            plan_receive(4096, 0, positions(8192, 8192, 8168), header(24, 1), || 8192).unwrap(),
            ReceivePlan::Lapped
        );
        assert_eq!(
            plan_receive(4096, 0, positions(24, 24, 0), header(4, 1), || 24)
                .unwrap_err()
                .reason(),
            "record"
        );
        assert_eq!(
            plan_receive(4096, 0, positions(24, 24, 0), header(4, 1), || 4096 + 8).unwrap(),
            ReceivePlan::Lapped,
            "a record that looks corrupt while being lapped is a lap"
        );
        assert_eq!(
            plan_receive(4096, 48, positions(24, 24, 0), header(24, 1), || 24).unwrap(),
            ReceivePlan::None,
            "a tail below the next record is a stale load, not a fault"
        );
        assert_eq!(
            plan_receive(4096, 0, positions(24, 24, 24), header(24, 1), || 24).unwrap(),
            ReceivePlan::Record {
                cursor: 0,
                index: 0,
                next_record: 24,
                header: RecordHeader::new(24, 1)
            },
            "a latest record at the tail is a transmit in progress"
        );
        assert_eq!(
            plan_receive(4096, 0, positions(8192, 8192, 0), header(24, 1), || 8192)
                .unwrap_err()
                .reason(),
            "tail",
            "a latest record more than a capacity behind the tail is a fault"
        );
    }
}
