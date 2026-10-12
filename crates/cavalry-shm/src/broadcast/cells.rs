//! The trailer and the ring as cells: the atomics the protocols of
//! `spec/broadcast-buffer.md` need, around the arithmetic of [`layout`]. As in
//! `counters::cells`, nothing here is constructed in production: `file` casts the
//! mapping's trailer to [`TrailerCells`] and its ring to words. Record bytes are written
//! and read a word at a time with relaxed atomic stores and loads, because a receiver
//! that has fallen behind reads what the transmitter is overwriting, by design, and the
//! tail intent tells it so afterwards. Under `cfg(loom)` the atomics are loom's and the
//! tests at the bottom check the transmit and receive protocols; `just loom` runs them.

// Under loom, `file` is not compiled (loom's atomics have another size), so the parts of
// the cells only `file` calls are unused there.
#![cfg_attr(loom, allow(dead_code))]

use super::BroadcastError;
use super::layout::{
    self, HeaderFields, MAGIC, PADDING_TYPE, Positions, RECORD_ALIGNMENT, RECORD_HEADER_LENGTH,
    ReceivePlan, TRAILER_LENGTH, is_lapped,
};
use crate::ring::{PositionCells, RingHeaderCells, Words, record_word};
use crate::sync::{Ordering, fence};

/// The 512-byte trailer: the header, then one slot per field at the spec's offsets.
#[repr(C, align(128))]
pub(crate) struct TrailerCells {
    pub(crate) header: RingHeaderCells,
    pub(crate) tail_intent: PositionCells,
    pub(crate) tail: PositionCells,
    pub(crate) latest: PositionCells,
}

// SAFETY: every field is `Sync` by its own argument in `crate::ring`.
unsafe impl Sync for TrailerCells {}

impl TrailerCells {
    /// Zero: a trailer the creator has not published. Tests only.
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            header: RingHeaderCells::zeroed(),
            tail_intent: PositionCells::zeroed(),
            tail: PositionCells::zeroed(),
            latest: PositionCells::zeroed(),
        }
    }

    /// The creator's protocol: the header's fields, then the magic with a release store.
    pub(crate) fn publish(&self, fields: &HeaderFields) {
        self.header.publish(MAGIC, fields.ring());
    }

    /// A reader's protocol: the magic with an acquire load, then the spec's checks for
    /// a file of `file_length` bytes, in order.
    pub(crate) fn read(&self, file_length: u64) -> Result<HeaderFields, BroadcastError> {
        self.header
            .read(MAGIC, file_length, TRAILER_LENGTH)
            .map(HeaderFields::from_ring)
            .map_err(layout::reject)
    }

    /// The three positions, each with an acquire load.
    pub(crate) fn positions(&self) -> Positions {
        Positions {
            tail: self.tail.load_acquire(),
            tail_intent: self.tail_intent.load_acquire(),
            latest: self.latest.load_acquire(),
        }
    }
}

/// Writes `message` into the ring from byte `start`, a word at a time with relaxed
/// stores; the last word's spare bytes are 0.
fn write_message(words: &impl Words, start: u32, message: &[u8]) {
    for (i, chunk) in message.chunks(RECORD_ALIGNMENT as usize).enumerate() {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        words
            .word(start + i as u32 * RECORD_ALIGNMENT)
            .store(u64::from_le_bytes(word), Ordering::Relaxed);
    }
}

/// Reads `out.len()` bytes of a message from byte `start`, a word at a time with relaxed
/// loads: the bytes may be torn, and the caller validates after.
fn read_message(words: &impl Words, start: u32, out: &mut [u8]) {
    for (i, chunk) in out.chunks_mut(RECORD_ALIGNMENT as usize).enumerate() {
        let word = words
            .word(start + i as u32 * RECORD_ALIGNMENT)
            .load(Ordering::Relaxed)
            .to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
}

/// The transmitter's protocol (`spec/broadcast-buffer.md`, "Transmitting") for a
/// `message` of type `type_id`: the tail intent with a release store and a release
/// fence, the padding record if any, the record, then the latest record and the tail
/// with release stores. [`BroadcastError::Type`] for a type below 1 and
/// [`BroadcastError::Message`] for a message over capacity ÷ 8, before anything is
/// written.
pub(crate) fn transmit(
    words: &impl Words,
    trailer: &TrailerCells,
    type_id: i32,
    message: &[u8],
) -> Result<(), BroadcastError> {
    if type_id < 1 {
        return Err(BroadcastError::Type { found: type_id });
    }
    let tail = trailer.tail.load_own();
    let plan = layout::plan_transmit(words.capacity(), tail, message.len())?;
    trailer.tail_intent.store_release(plan.tail_intent);
    fence(Ordering::Release);
    if let Some(padding) = plan.padding {
        words.word(padding.index).store(
            record_word(padding.length as i32, PADDING_TYPE),
            Ordering::Relaxed,
        );
    }
    words.word(plan.index).store(
        record_word(plan.record_length as i32, type_id),
        Ordering::Relaxed,
    );
    write_message(words, plan.index + RECORD_HEADER_LENGTH, message);
    trailer.latest.store_release(plan.latest);
    trailer.tail.store_release(plan.tail_after);
    Ok(())
}

/// A receiver's place in the stream: the record it last read and the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReceiverState {
    pub(crate) cursor: i64,
    pub(crate) next_record: i64,
}

/// What one receive did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Received {
    /// Nothing is available.
    None,
    /// The receiver was lapped and now sits at the latest record.
    Lapped {
        cursor: i64,
        tail_intent: i64,
        latest: i64,
    },
    /// A message of this type and length is in the caller's scratch buffer.
    Message { type_id: i32, length: usize },
}

/// Joining: the cursor and the next record are the latest record, so the first receive
/// delivers it.
pub(crate) fn join(trailer: &TrailerCells) -> ReceiverState {
    let latest = trailer.latest.load_acquire();
    ReceiverState {
        cursor: latest,
        next_record: latest,
    }
}

/// The receiver's protocol (`spec/broadcast-buffer.md`, "Receiving"): the positions
/// with acquire loads and their checks, the lap check, the record's header and its
/// checks, the copy into `scratch`, an acquire fence, and the tail intent again.
pub(crate) fn receive(
    words: &impl Words,
    trailer: &TrailerCells,
    state: &mut ReceiverState,
    scratch: &mut [u8],
) -> Result<Received, BroadcastError> {
    let capacity = words.capacity();
    let positions = trailer.positions();
    let load_tail_intent = || {
        fence(Ordering::Acquire);
        trailer.tail_intent.load_acquire()
    };
    let plan = layout::plan_receive(
        capacity,
        state.next_record,
        positions,
        |index| words.word(index).load(Ordering::Relaxed),
        load_tail_intent,
    )?;
    let lapped = |state: &mut ReceiverState, cursor: i64, tail_intent: i64| {
        let latest = trailer.latest.load_acquire();
        state.cursor = latest;
        state.next_record = latest;
        Received::Lapped {
            cursor,
            tail_intent,
            latest,
        }
    };
    match plan {
        ReceivePlan::None => Ok(Received::None),
        ReceivePlan::Lapped => Ok(lapped(state, state.next_record, positions.tail_intent)),
        ReceivePlan::Record {
            cursor,
            index,
            next_record,
            header,
        } => {
            let length = header.message_length() as usize;
            read_message(words, index + RECORD_HEADER_LENGTH, &mut scratch[..length]);
            let tail_intent = load_tail_intent();
            if is_lapped(cursor, capacity, tail_intent) {
                return Ok(lapped(state, cursor, tail_intent));
            }
            state.cursor = cursor;
            state.next_record = next_record;
            Ok(Received::Message {
                type_id: header.type_id(),
                length,
            })
        }
    }
}

// The spec's offsets and sizes, checked at compile time; under loom the atomics have
// another size and the protocols, not the layout, are under test.
#[cfg(not(loom))]
const _: () = {
    assert!(size_of::<TrailerCells>() == TRAILER_LENGTH);
    assert!(align_of::<TrailerCells>() == 128);
    assert!(core::mem::offset_of!(TrailerCells, tail_intent) == layout::TAIL_INTENT_OFFSET);
    assert!(core::mem::offset_of!(TrailerCells, tail) == layout::TAIL_POSITION_OFFSET);
    assert!(core::mem::offset_of!(TrailerCells, latest) == layout::LATEST_RECORD_OFFSET);
};

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::ring::WordVec;

    #[test]
    fn a_receiver_joins_at_the_latest_and_reads_in_order_across_the_end() {
        let words = WordVec::zeroed(64);
        let trailer = TrailerCells::zeroed();
        let mut scratch = [0u8; 8];
        let mut state = join(&trailer);
        assert_eq!(
            receive(&words, &trailer, &mut state, &mut scratch).unwrap(),
            Received::None
        );
        transmit(&words, &trailer, 1, &[1; 8]).unwrap(); // 16 bytes at 0
        transmit(&words, &trailer, 2, &[2; 8]).unwrap(); // 16 at 16
        transmit(&words, &trailer, 3, &[]).unwrap(); // 8 at 32
        transmit(&words, &trailer, 4, &[4; 5]).unwrap(); // 13, aligned 16, at 40
        assert_eq!(
            trailer.positions(),
            Positions {
                tail: 56,
                tail_intent: 56,
                latest: 40
            }
        );
        for (type_id, len) in [(1, 8), (2, 8), (3, 0), (4, 5)] {
            assert_eq!(
                receive(&words, &trailer, &mut state, &mut scratch).unwrap(),
                Received::Message {
                    type_id,
                    length: len
                }
            );
            assert!(scratch[..len].iter().all(|b| i32::from(*b) == type_id));
        }
        // 16 bytes do not fit the 8 to the end: padding there, then the record at 0.
        transmit(&words, &trailer, 5, &[5; 8]).unwrap();
        assert_eq!(
            trailer.positions(),
            Positions {
                tail: 80,
                tail_intent: 80,
                latest: 64
            }
        );
        assert_eq!(
            receive(&words, &trailer, &mut state, &mut scratch).unwrap(),
            Received::Message {
                type_id: 5,
                length: 8
            }
        );
        assert_eq!(
            state,
            ReceiverState {
                cursor: 64,
                next_record: 80
            }
        );
        assert_eq!(
            transmit(&words, &trailer, 0, &[]).unwrap_err().reason(),
            "type"
        );
        assert_eq!(
            transmit(&words, &trailer, 1, &[0; 9]).unwrap_err().reason(),
            "message"
        );
    }

    #[test]
    fn a_lapped_receiver_is_told_and_continues_from_the_latest() {
        let words = WordVec::zeroed(64);
        let trailer = TrailerCells::zeroed();
        let mut scratch = [0u8; 8];
        let mut state = join(&trailer);
        for n in 1..=5 {
            transmit(&words, &trailer, n, &[n as u8; 8]).unwrap();
        }
        assert_eq!(
            trailer.positions(),
            Positions {
                tail: 80,
                tail_intent: 80,
                latest: 64
            }
        );
        assert_eq!(
            receive(&words, &trailer, &mut state, &mut scratch).unwrap(),
            Received::Lapped {
                cursor: 0,
                tail_intent: 80,
                latest: 64
            }
        );
        assert_eq!(
            receive(&words, &trailer, &mut state, &mut scratch).unwrap(),
            Received::Message {
                type_id: 5,
                length: 8
            }
        );
        assert_eq!(
            receive(&words, &trailer, &mut state, &mut scratch).unwrap(),
            Received::None
        );
    }
}

#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::thread;

    use super::*;
    use crate::ring::WordVec;

    /// The transmitter writes a record and releases the tail; a receiver that finds the
    /// tail moved delivers that record whole, and never a torn one.
    #[test]
    fn a_receiver_acquires_the_record_the_transmitter_released() {
        loom::model(|| {
            let words = Arc::new(WordVec::zeroed(32));
            let trailer = Arc::new(TrailerCells::zeroed());
            let transmitter = {
                let words = Arc::clone(&words);
                let trailer = Arc::clone(&trailer);
                thread::spawn(move || transmit(&*words, &*trailer, 3, &[3; 4]).unwrap())
            };
            let mut state = join(&trailer);
            let mut scratch = [0u8; 4];
            loop {
                match receive(&*words, &*trailer, &mut state, &mut scratch).unwrap() {
                    Received::None => thread::yield_now(),
                    Received::Message { type_id, length } => {
                        assert_eq!((type_id, length), (3, 4));
                        assert_eq!(scratch, [3; 4]);
                        break;
                    }
                    Received::Lapped { .. } => panic!("one record cannot lap a receiver"),
                }
            }
            transmitter.join().unwrap();
        });
    }

    /// The transmitter laps a receiver that reads concurrently: every record the
    /// receiver delivers is whole, its message matching its type, and the lap is told,
    /// never delivered as a torn record. A 32-byte ring holds two 16-byte records, so
    /// the third overwrites the first. The receiver makes four attempts while the
    /// transmitter runs, then drains what is left once it has finished; the model is
    /// bounded to two preemptions, since a receiver that spins to the end has no bound.
    #[test]
    fn a_lapped_receiver_never_delivers_a_torn_record() {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.max_branches = 50_000;
        model.check(|| {
            let words = Arc::new(WordVec::zeroed(32));
            let trailer = Arc::new(TrailerCells::zeroed());
            let mut state = join(&trailer);
            let transmitter = {
                let words = Arc::clone(&words);
                let trailer = Arc::clone(&trailer);
                thread::spawn(move || {
                    for n in 1..=3u8 {
                        transmit(&*words, &*trailer, i32::from(n), &[n; 4]).unwrap();
                    }
                })
            };
            let mut scratch = [0u8; 4];
            let mut delivered = Vec::new();
            let mut check = |received: Received, scratch: &[u8; 4]| match received {
                Received::None => false,
                Received::Lapped { .. } => true,
                Received::Message { type_id, length } => {
                    assert_eq!(length, 4);
                    assert!(
                        scratch.iter().all(|b| i32::from(*b) == type_id),
                        "{scratch:?} {type_id}"
                    );
                    delivered.push(type_id);
                    true
                }
            };
            for _ in 0..4 {
                let received = receive(&*words, &*trailer, &mut state, &mut scratch).unwrap();
                if !check(received, &scratch) {
                    thread::yield_now();
                }
            }
            transmitter.join().unwrap();
            loop {
                let received = receive(&*words, &*trailer, &mut state, &mut scratch).unwrap();
                if !check(received, &scratch) {
                    break;
                }
            }
            assert_eq!(
                delivered.last(),
                Some(&3),
                "the last record is always delivered"
            );
            assert!(
                delivered.windows(2).all(|w| w[0] < w[1]),
                "in order: {delivered:?}"
            );
        });
    }
}
