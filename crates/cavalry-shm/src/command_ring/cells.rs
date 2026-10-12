//! The trailer and the ring as cells: the atomics the protocols of
//! `spec/command-ring.md` need, around the arithmetic of [`layout`]. As in
//! `counters::cells`, nothing here is constructed in production: `file` casts the
//! mapping's trailer to [`TrailerCells`] and its ring to words, so every struct is
//! `#[repr(C)]` at the spec's offsets and holds nothing but atomics and reserved bytes.
//! Under `cfg(loom)` the atomics are loom's and the tests at the bottom check the claim,
//! commit, read and unblock protocols; `just loom` runs them.

// Under loom, `file` is not compiled (loom's atomics have another size), so the parts of
// the cells only `file` calls are unused there.
#![cfg_attr(loom, allow(dead_code))]

use super::CommandRingError;
use super::layout::{
    self, ClaimAction, ClaimOutcome, ClaimResult, DRIVER_CLOSED, HeaderFields, MAGIC, PADDING_TYPE,
    PaddingRecord, RECORD_ALIGNMENT, ReadPlan, RecordHeader, TRAILER_LENGTH,
};
use crate::ring::{PositionCells, RingHeaderCells, Words, record_word};
use crate::sync::Ordering;

/// The 768-byte trailer: the header, then one slot per field at the spec's offsets.
#[repr(C, align(128))]
pub(crate) struct TrailerCells {
    pub(crate) header: RingHeaderCells,
    pub(crate) tail: PositionCells,
    pub(crate) head_cache: PositionCells,
    pub(crate) head: PositionCells,
    pub(crate) correlation: PositionCells,
    pub(crate) heartbeat: PositionCells,
}

// SAFETY: every field is `Sync` by its own argument in `crate::ring`.
unsafe impl Sync for TrailerCells {}

impl TrailerCells {
    /// Zero: a trailer the creator has not published. Tests only.
    #[cfg(any(test, loom))]
    pub(crate) fn zeroed() -> Self {
        Self {
            header: RingHeaderCells::zeroed(),
            tail: PositionCells::zeroed(),
            head_cache: PositionCells::zeroed(),
            head: PositionCells::zeroed(),
            correlation: PositionCells::zeroed(),
            heartbeat: PositionCells::zeroed(),
        }
    }

    /// The creator's protocol: the heartbeat and the slots were stored before this; the
    /// header's fields, then the magic with a release store.
    pub(crate) fn publish(&self, fields: &HeaderFields) {
        self.header.publish(MAGIC, layout::VERSION, fields.ring());
    }

    /// A reader's protocol: the magic with an acquire load, then the spec's checks for
    /// a file of `file_length` bytes, in order.
    pub(crate) fn read(&self, file_length: u64) -> Result<HeaderFields, CommandRingError> {
        self.header
            .read(MAGIC, layout::VERSION, file_length, TRAILER_LENGTH)
            .map(HeaderFields::from_ring)
            .map_err(layout::reject)
    }
}

/// A record a producer holds between claim and commit: where it is and what its word
/// says once committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Claimed {
    pub(crate) index: u32,
    pub(crate) record_length: u32,
    pub(crate) type_id: i32,
}

/// Steps 2 to 7 of a claim (`spec/command-ring.md`, "Claim, commit and abort") for a
/// message of `message_length` bytes and type `type_id`: the compare-and-swap loop on
/// the tail, the head cache, the padding record, and the claim's negative word.
pub(crate) fn claim(
    words: &impl Words,
    trailer: &TrailerCells,
    type_id: i32,
    message_length: usize,
) -> Result<Claimed, CommandRingError> {
    if type_id < 1 {
        return Err(CommandRingError::Type { found: type_id });
    }
    loop {
        let head_cache = trailer.head_cache.load_acquire();
        let tail = trailer.tail.load_acquire();
        let plan = layout::plan_claim(words.capacity(), tail, head_cache, message_length, || {
            trailer.head.load_acquire()
        })?;
        if let Some(head) = plan.head_cache_after {
            trailer.head_cache.store_release(head);
        }
        if plan.result == ClaimResult::Full && plan.padding.is_none() {
            return Err(CommandRingError::Full {
                needed: plan.aligned_length,
            });
        }
        if trailer
            .tail
            .compare_exchange(tail, plan.tail_after)
            .is_err()
        {
            continue;
        }
        if let Some(padding) = plan.padding {
            // Step 6: a failed swap means the driver padded this gap meanwhile, over
            // the same bytes, and there is nothing left to write.
            let _ = pad_from(words, padding, 0);
        }
        return match plan.result {
            ClaimResult::Full => Err(CommandRingError::Full {
                needed: plan.aligned_length,
            }),
            ClaimResult::Claimed { index } => {
                // Step 7: the claim word, swapped in from 0. A failed swap means the
                // driver padded a gap over this index before the claim word landed.
                let negative = -(plan.record_length as i32);
                words
                    .word(index)
                    .compare_exchange(
                        0,
                        record_word(negative, type_id),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .map_err(|_| CommandRingError::Unblocked { index })?;
                Ok(Claimed {
                    index,
                    record_length: plan.record_length,
                    type_id,
                })
            }
        };
    }
}

/// A padding record's length and type −1, swapped in from the word `expected` that its
/// writer loaded; `false` when another party changed the word first, and nothing is
/// written.
fn pad_from(words: &impl Words, padding: PaddingRecord, expected: u64) -> bool {
    words
        .word(padding.index)
        .compare_exchange(
            expected,
            record_word(padding.length as i32, PADDING_TYPE),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

/// The claim word: the negative length and the type the producer stored when it claimed.
fn claim_word(claimed: &Claimed) -> RecordHeader {
    RecordHeader::new(-(claimed.record_length as i32), claimed.type_id)
}

/// Step 8 of a claim (`spec/command-ring.md`, "Claim, commit and abort"): the acquire
/// load before the message is written. A word that is not the claim word is
/// [`CommandRingError::Unblocked`]: the driver padded the claim, and nothing is written.
pub(crate) fn check_claim(words: &impl Words, claimed: &Claimed) -> Result<(), CommandRingError> {
    let found = RecordHeader::from_word(words.word(claimed.index).load(Ordering::Acquire));
    match layout::plan_claim_action(claim_word(claimed), found, ClaimAction::Write) {
        ClaimOutcome::Writable => Ok(()),
        _ => Err(CommandRingError::Unblocked {
            index: claimed.index,
        }),
    }
}

/// Step 9: the compare-and-swap from the claim word to the record's header for `action`,
/// release on success, which publishes the message written before it, acquire on
/// failure. A failed swap is [`CommandRingError::Unblocked`], and nothing is stored.
fn swap(
    words: &impl Words,
    claimed: &Claimed,
    action: ClaimAction,
) -> Result<(), CommandRingError> {
    let claim = claim_word(claimed);
    let after = match layout::plan_claim_action(claim, claim, action) {
        ClaimOutcome::Committed(header) | ClaimOutcome::Aborted(header) => header,
        ClaimOutcome::Claimed(_) | ClaimOutcome::Writable | ClaimOutcome::Unblocked => {
            unreachable!("a swap commits or aborts")
        }
    };
    words
        .word(claimed.index)
        .compare_exchange(
            claim.word(),
            after.word(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map(|_| ())
        .map_err(|_| CommandRingError::Unblocked {
            index: claimed.index,
        })
}

/// Commit: the positive length and the type, swapped in from the claim word.
pub(crate) fn commit(words: &impl Words, claimed: &Claimed) -> Result<(), CommandRingError> {
    swap(words, claimed, ClaimAction::Commit)
}

/// Abort: the positive length and type −1, swapped in from the claim word; the consumer
/// skips the record.
pub(crate) fn abort(words: &impl Words, claimed: &Claimed) -> Result<(), CommandRingError> {
    swap(words, claimed, ClaimAction::Abort)
}

/// The ring's words with acquire loads, as the read arithmetic wants them.
struct Acquire<'a, W: Words>(&'a W);

impl<W: Words> layout::RecordWords for Acquire<'_, W> {
    fn record_word(&self, index: u32) -> u64 {
        self.0.word(index).load(Ordering::Acquire)
    }
}

/// The consumer's read (`spec/command-ring.md`, "Reading"): up to `limit` messages
/// from the head, each given to `on_record` as its index and header before anything is
/// cleared; then the bytes read are zeroed and the head is released.
pub(crate) fn read(
    words: &impl Words,
    trailer: &TrailerCells,
    limit: usize,
    on_record: impl FnMut(u32, RecordHeader),
) -> ReadPlan {
    let head = trailer.head.load_own();
    let plan = layout::plan_read(words.capacity(), head, limit, &Acquire(words), on_record);
    if plan.bytes_read > 0 {
        let head_index = (head as u64 & u64::from(words.capacity() - 1)) as u32;
        let mut index = head_index;
        while index < head_index + plan.bytes_read {
            words.word(index).store(0, Ordering::Relaxed);
            index += RECORD_ALIGNMENT;
        }
        trailer
            .head
            .store_release(head + i64::from(plan.bytes_read));
    }
    plan
}

/// The driver's unblock (`spec/command-ring.md`, "Unblocking"): the padding record it
/// wrote, or `None` when nothing was blocked, or when the producer committed, aborted
/// or claimed there between the driver's load and its swap, in which case nothing is
/// written and the next read or unblock finds the new word (ADR 0018).
pub(crate) fn unblock(
    words: &impl Words,
    trailer: &TrailerCells,
) -> Result<Option<PaddingRecord>, CommandRingError> {
    let head = trailer.head.load_acquire();
    let tail = trailer.tail.load_acquire();
    let consumer_index = (head as u64 & u64::from(words.capacity() - 1)) as u32;
    let mut observed = 0u64;
    let plan = layout::plan_unblock(words.capacity(), head, tail, |index| {
        let word = words.word(index).load(Ordering::Acquire);
        if index == consumer_index {
            observed = word;
        }
        word
    })?;
    match plan {
        Some(padding) if pad_from(words, padding, observed) => Ok(Some(padding)),
        _ => Ok(None),
    }
}

/// Step 1 of a claim: the consumer heartbeat against the producer's clock `now` and the
/// driver `timeout`, both in nanoseconds.
pub(crate) fn check_driver(
    trailer: &TrailerCells,
    now: i64,
    timeout: i64,
) -> Result<(), CommandRingError> {
    let heartbeat = trailer.heartbeat.load_acquire();
    if heartbeat == DRIVER_CLOSED {
        return Err(CommandRingError::DriverClosed);
    }
    let age = now.saturating_sub(heartbeat);
    if age > timeout {
        return Err(CommandRingError::DriverTimeout { age, timeout });
    }
    Ok(())
}

// The spec's offsets and sizes, checked at compile time; under loom the atomics have
// another size and the protocols, not the layout, are under test.
#[cfg(not(loom))]
const _: () = {
    assert!(size_of::<TrailerCells>() == TRAILER_LENGTH);
    assert!(align_of::<TrailerCells>() == 128);
    assert!(core::mem::offset_of!(TrailerCells, tail) == layout::TAIL_POSITION_OFFSET);
    assert!(core::mem::offset_of!(TrailerCells, head_cache) == layout::HEAD_CACHE_POSITION_OFFSET);
    assert!(core::mem::offset_of!(TrailerCells, head) == layout::HEAD_POSITION_OFFSET);
    assert!(
        core::mem::offset_of!(TrailerCells, correlation) == layout::CORRELATION_ID_COUNTER_OFFSET
    );
    assert!(core::mem::offset_of!(TrailerCells, heartbeat) == layout::CONSUMER_HEARTBEAT_OFFSET);
};

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::ring::WordVec;

    #[test]
    fn a_claim_commits_and_a_read_takes_it_then_clears_it() {
        let words = WordVec::zeroed(64);
        let trailer = TrailerCells::zeroed();
        let claimed = claim(&words, &trailer, 7, 8).unwrap();
        assert_eq!(
            claimed,
            Claimed {
                index: 0,
                record_length: 16,
                type_id: 7
            }
        );
        assert_eq!(trailer.tail.load_acquire(), 16);
        let plan = read(&words, &trailer, 16, |_, _| {
            panic!("a claim in progress is not read")
        });
        assert_eq!(plan.stop, layout::ReadStop::Claimed);
        words.word(8).store(0xABCD, Ordering::Relaxed);
        commit(&words, &claimed).unwrap();
        let mut seen = None;
        let plan = read(&words, &trailer, 16, |index, header| {
            seen = Some((index, header))
        });
        assert_eq!(seen, Some((0, RecordHeader::new(16, 7))));
        assert_eq!((plan.bytes_read, plan.messages), (16, 1));
        assert_eq!(trailer.head.load_acquire(), 16);
        assert_eq!(words.word(0).load(Ordering::Relaxed), 0);
        assert_eq!(words.word(8).load(Ordering::Relaxed), 0, "cleared");
    }

    #[test]
    fn the_full_ring_wraps_with_padding_and_an_unblock_frees_a_dead_claim() {
        let words = WordVec::zeroed(64);
        let trailer = TrailerCells::zeroed();
        commit(&words, &claim(&words, &trailer, 2, 0).unwrap()).unwrap(); // 8 bytes at 0
        for _ in 0..3 {
            commit(&words, &claim(&words, &trailer, 1, 8).unwrap()).unwrap(); // 16 each, to 56
        }
        assert_eq!(trailer.tail.load_acquire(), 56);
        // With 8 free bytes in all, a 16-byte record is refused outright and nothing moves.
        let full = claim(&words, &trailer, 1, 8).unwrap_err();
        assert_eq!(full.reason(), "full");
        assert_eq!(trailer.tail.load_acquire(), 56);
        // With 16 free bytes, 8 at the end and 8 at the start, the record does not fit
        // either place: the 8 to the end become padding and the result is still full.
        let plan = read(&words, &trailer, 1, |_, header| {
            assert_eq!(header.type_id(), 2)
        });
        assert_eq!((plan.messages, plan.bytes_read), (1, 8));
        let full = claim(&words, &trailer, 1, 8).unwrap_err();
        assert_eq!(full.reason(), "full");
        assert_eq!(
            trailer.tail.load_acquire(),
            64,
            "the padding to the end was claimed"
        );
        assert_eq!(
            RecordHeader::from_word(words.word(56).load(Ordering::Relaxed)),
            RecordHeader::new(8, -1)
        );
        let plan = read(&words, &trailer, 16, |_, _| {});
        assert_eq!((plan.messages, plan.bytes_read), (3, 56));
        assert_eq!(plan.stop, layout::ReadStop::EndOfRing);

        let dead = claim(&words, &trailer, 1, 8).unwrap();
        assert_eq!(dead.index, 0);
        assert_eq!(
            unblock(&words, &trailer).unwrap(),
            Some(PaddingRecord {
                index: 0,
                length: 16
            })
        );
        let plan = read(&words, &trailer, 16, |_, _| {
            panic!("padding delivers nothing")
        });
        assert_eq!((plan.messages, plan.bytes_read), (0, 16));
        assert_eq!(unblock(&words, &trailer).unwrap(), None);
        assert_eq!(claim(&words, &trailer, 0, 8).unwrap_err().reason(), "type");
        assert_eq!(
            claim(&words, &trailer, 1, 9).unwrap_err().reason(),
            "message"
        );
    }

    #[test]
    fn the_heartbeat_tells_a_closed_driver_from_a_stopped_one() {
        let trailer = TrailerCells::zeroed();
        trailer.heartbeat.store_release(1_000);
        assert!(check_driver(&trailer, 1_500, 1_000).is_ok());
        assert_eq!(
            check_driver(&trailer, 2_001, 1_000).unwrap_err().reason(),
            "driver-timeout"
        );
        trailer.heartbeat.store_release(DRIVER_CLOSED);
        assert_eq!(
            check_driver(&trailer, 1_000, 1_000).unwrap_err().reason(),
            "driver-closed"
        );
    }
}

#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::thread;

    use super::*;
    use crate::ring::WordVec;

    /// A producer claims, writes its message word and commits; the consumer, reading
    /// concurrently, delivers the record exactly once with the message the producer
    /// wrote, and never a claim in progress. loom explores every interleaving.
    #[test]
    fn a_consumer_acquires_the_record_the_producer_committed() {
        loom::model(|| {
            let words = Arc::new(WordVec::zeroed(64));
            let trailer = Arc::new(TrailerCells::zeroed());
            let producer = {
                let words = Arc::clone(&words);
                let trailer = Arc::clone(&trailer);
                thread::spawn(move || {
                    let claimed = claim(&*words, &*trailer, 7, 8).unwrap();
                    words
                        .word(claimed.index + 8)
                        .store(0xABCD, Ordering::Relaxed);
                    commit(&*words, &claimed).unwrap();
                })
            };
            loop {
                let mut seen = None;
                read(&*words, &*trailer, 16, |index, header| {
                    seen = Some((index, header, words.word(index + 8).load(Ordering::Relaxed)));
                });
                if let Some((index, header, message)) = seen {
                    assert_eq!((index, header), (0, RecordHeader::new(16, 7)));
                    assert_eq!(message, 0xABCD);
                    break;
                }
                thread::yield_now();
            }
            producer.join().unwrap();
            assert_eq!(trailer.head.load_acquire(), 16);
            assert_eq!(trailer.tail.load_acquire(), 16);
        });
    }

    /// Two producers claim at once: the compare-and-swap gives them disjoint records
    /// and the tail advances by both.
    #[test]
    fn two_producers_claim_disjoint_records() {
        loom::model(|| {
            let words = Arc::new(WordVec::zeroed(64));
            let trailer = Arc::new(TrailerCells::zeroed());
            let other = {
                let words = Arc::clone(&words);
                let trailer = Arc::clone(&trailer);
                thread::spawn(move || claim(&*words, &*trailer, 1, 8).unwrap().index)
            };
            let mine = claim(&*words, &*trailer, 2, 8).unwrap().index;
            let theirs = other.join().unwrap();
            assert!(
                (mine == 0 && theirs == 16) || (mine == 16 && theirs == 0),
                "{mine} {theirs}"
            );
            assert_eq!(trailer.tail.load_acquire(), 32);
        });
    }

    /// A producer writes three records while the consumer reads: every record is
    /// delivered once, in order, with its message, whatever the consumer's head and head
    /// cache said when the producer loaded them. A 32-byte ring holds two 16-byte
    /// records of 4-byte messages, so the third waits for the first to be cleared. The
    /// two threads spin on each other, so the model is bounded to two preemptions.
    #[test]
    fn a_producer_and_the_consumer_run_concurrently() {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.max_branches = 50_000;
        model.check(|| {
            let words = Arc::new(WordVec::zeroed(32));
            let trailer = Arc::new(TrailerCells::zeroed());
            let producer = {
                let words = Arc::clone(&words);
                let trailer = Arc::clone(&trailer);
                thread::spawn(move || {
                    for n in 1..=3u64 {
                        let claimed = loop {
                            match claim(&*words, &*trailer, n as i32, 4) {
                                Ok(claimed) => break claimed,
                                Err(CommandRingError::Full { .. }) => thread::yield_now(),
                                Err(other) => panic!("{other}"),
                            }
                        };
                        words
                            .word(claimed.index + 8)
                            .store(n * 0x1111, Ordering::Relaxed);
                        commit(&*words, &claimed).unwrap();
                    }
                })
            };
            let mut delivered = Vec::new();
            while delivered.len() < 3 {
                let before = delivered.len();
                read(&*words, &*trailer, 16, |index, header| {
                    delivered.push((
                        header.type_id(),
                        words.word(index + 8).load(Ordering::Relaxed),
                    ));
                });
                if delivered.len() == before {
                    thread::yield_now();
                }
            }
            producer.join().unwrap();
            assert_eq!(delivered, vec![(1, 0x1111), (2, 0x2222), (3, 0x3333)]);
            assert_eq!(trailer.head.load_acquire(), 48);
            assert_eq!(trailer.tail.load_acquire(), 48);
        });
    }

    /// A producer claims and dies before it commits, at any point of its claim, and a
    /// second producer then claims and commits; the driver's unblock and read, run until
    /// the head catches the tail, deliver the second record once and leave the ring
    /// free. The dead claim is found as a negative length, or as a zero gap bounded by
    /// the second record.
    #[test]
    fn a_dead_claim_never_blocks_the_ring() {
        loom::model(|| {
            let words = Arc::new(WordVec::zeroed(64));
            let trailer = Arc::new(TrailerCells::zeroed());
            let dead = {
                let words = Arc::clone(&words);
                let trailer = Arc::clone(&trailer);
                thread::spawn(move || {
                    claim(&*words, &*trailer, 1, 8).unwrap();
                })
            };
            let live = {
                let words = Arc::clone(&words);
                let trailer = Arc::clone(&trailer);
                thread::spawn(move || {
                    let claimed = claim(&*words, &*trailer, 2, 8).unwrap();
                    commit(&*words, &claimed).unwrap();
                })
            };
            dead.join().unwrap();
            live.join().unwrap();
            let mut delivered = Vec::new();
            loop {
                unblock(&*words, &*trailer).unwrap();
                read(&*words, &*trailer, 16, |_, header| {
                    delivered.push(header.type_id())
                });
                if trailer.head.load_acquire() == trailer.tail.load_acquire() {
                    break;
                }
                thread::yield_now();
            }
            assert_eq!(
                delivered,
                vec![2],
                "the dead claim delivers nothing, the live one once"
            );
            assert_eq!(trailer.head.load_acquire(), 32);
        });
    }

    /// A producer claims, checks its claim, writes its message and commits, while the
    /// driver unblocks and reads, in every interleaving (ADR 0018): the record is
    /// delivered exactly when the producer's commit succeeded, a producer whose index
    /// or claim was padded reports `unblocked` at the claim, at the check or at the swap
    /// and stores nothing more, and the ring ends empty with the slot cleared, never
    /// with a positive header behind the head and never with a committed record under
    /// a padding. Before the unblock swapped, the model found the second: a padding
    /// store landing on a record committed an instant earlier.
    #[test]
    fn a_late_commit_after_an_unblock_is_refused() {
        loom::model(|| {
            let words = Arc::new(WordVec::zeroed(64));
            let trailer = Arc::new(TrailerCells::zeroed());
            let producer = {
                let words = Arc::clone(&words);
                let trailer = Arc::clone(&trailer);
                thread::spawn(move || {
                    let outcome = claim(&*words, &*trailer, 1, 8).and_then(|claimed| {
                        assert_eq!(claimed.index, 0);
                        check_claim(&*words, &claimed)?;
                        words
                            .word(claimed.index + 8)
                            .store(0xABCD, Ordering::Relaxed);
                        commit(&*words, &claimed)
                    });
                    match outcome {
                        Ok(()) => true,
                        Err(CommandRingError::Unblocked { index }) => {
                            assert_eq!(index, 0);
                            false
                        }
                        Err(other) => panic!("{other}"),
                    }
                })
            };
            let mut delivered = Vec::new();
            let mut trace = Vec::new();
            loop {
                let unblocked = unblock(&*words, &*trailer).unwrap();
                let plan = read(&*words, &*trailer, 16, |index, header| {
                    delivered.push((index, header, words.word(index + 8).load(Ordering::Relaxed)));
                });
                // The ring is empty before the producer claims and after the consumer
                // has read or skipped its record; only the second ends the loop.
                let (head, tail) = (trailer.head.load_acquire(), trailer.tail.load_acquire());
                trace.push(format!(
                    "unblock {unblocked:?}; read {:?}; head {head} tail {tail} word0 {:?} word8 {:#x}",
                    plan,
                    RecordHeader::from_word(words.word(0).load(Ordering::Acquire)),
                    words.word(8).load(Ordering::Acquire)
                ));
                if tail > 0 && head == tail {
                    break;
                }
                thread::yield_now();
            }
            let committed = producer.join().unwrap();
            if committed {
                assert_eq!(
                    delivered,
                    vec![(0, RecordHeader::new(16, 1), 0xABCD)],
                    "committed but not delivered; trace: {trace:#?}"
                );
            } else {
                assert_eq!(
                    delivered,
                    vec![],
                    "an unblocked claim delivers nothing; trace: {trace:#?}"
                );
            }
            assert_eq!(trailer.head.load_acquire(), 16);
            assert_eq!(trailer.tail.load_acquire(), 16);
            assert_eq!(
                words.word(0).load(Ordering::Acquire),
                0,
                "the slot is cleared, with no late header behind the head"
            );
        });
    }
}
