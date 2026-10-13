//! A bounded queue between two threads of one process: one producer, one consumer, no
//! lock, no allocation after construction (CLAUDE.md, "Data plane": lock-free SPSC
//! queues; invariant 3: a fixed capacity and a stated behaviour when full). The client's
//! conductor thread hands the user's thread the images the driver announced through one
//! of these, and gets the mappings back through another, so that neither thread opens,
//! unmaps or waits on the other (ADR 0021). In Agrona, this is
//! `OneToOneConcurrentArrayQueue`, with `offer` and `poll` (`docs/glossary.md`).
//!
//! The protocol is the classic one: the producer owns `tail`, the consumer owns `head`,
//! both count up without wrapping, and a slot is `position % capacity`. The producer
//! writes a slot and then stores `tail` with a release store; the consumer loads `tail`
//! with an acquire load and reads only slots below it, then stores `head` with a release
//! store; the producer loads `head` with an acquire load and writes only slots whose
//! previous value the consumer has taken. The two counters sit in 128-byte slots of
//! their own (ADR 0011). Under `cfg(loom)` the atomics and cells are loom's, and the
//! models at the end of this file explore every interleaving.
//!
//! ```
//! use cavalry_shm::spsc;
//!
//! let (mut producer, mut consumer) = spsc::queue::<u64>(2);
//! assert_eq!(producer.offer(1), Ok(()));
//! assert_eq!(producer.offer(2), Ok(()));
//! // Full: the item comes back to the caller, nothing grows (invariant 3).
//! assert_eq!(producer.offer(3), Err(3));
//! let consumer_thread = std::thread::spawn(move || {
//!     let mut seen = Vec::new();
//!     while seen.len() < 2 {
//!         if let Some(item) = consumer.poll() {
//!             seen.push(item);
//!         }
//!     }
//!     seen
//! });
//! assert_eq!(consumer_thread.join().unwrap(), vec![1, 2]);
//! ```

use core::mem::MaybeUninit;
use std::sync::Arc;

use crate::sync::{AtomicUsize, Ordering, UnsafeCell};

/// A counter in a 128-byte slot of its own: the line pair a core moves at once, so that
/// the producer's `tail` and the consumer's `head` never share one (ADR 0011).
#[repr(C, align(128))]
struct Padded(AtomicUsize);

/// The queue both halves share.
struct Shared<T> {
    /// The next position the consumer reads; written by the consumer only.
    head: Padded,
    /// The next position the producer writes; written by the producer only.
    tail: Padded,
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
}

// SAFETY: a slot is written by the producer before it releases `tail` and read by the
// consumer after it acquires `tail`, and written again only after the consumer released
// `head` past it and the producer acquired `head`. There is exactly one producer and one
// consumer per queue: `queue` makes one `Producer` and one `Consumer`, neither is
// `Clone`, and a slot is reached only from `Producer::offer` and `Consumer::poll`, which
// take `&mut self`, so a shared `&Producer` or `&Consumer` on another thread (the halves
// are `Sync` when `T: Send`) can only load the counters and read the half's own fields.
// `T` must be `Send` for an item to cross threads.
unsafe impl<T: Send> Send for Shared<T> {}
// SAFETY: as above; the halves hold an `Arc<Shared<T>>` from two threads at once, the
// `&self` methods only load the atomic counters, and every access to a slot is through
// `offer` or `poll`, one caller each, in the protocol the counters order.
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Shared<T> {
    fn capacity(&self) -> usize {
        self.slots.len()
    }

    fn slot(&self, position: usize) -> &UnsafeCell<MaybeUninit<T>> {
        // The modulo is on a non-zero capacity, checked in `queue`; the index is in range.
        &self.slots[position % self.capacity()]
    }
}

impl<T> Drop for Shared<T> {
    /// Drops the items still queued: the ones from `head` up to `tail`. Both halves are
    /// gone when this runs, so no store or load races it.
    fn drop(&mut self) {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        for position in head..tail {
            // SAFETY: a slot between `head` and `tail` holds an item the producer wrote
            // and the consumer has not taken, and nothing else can reach it now.
            self.slot(position)
                .with_mut(|cell| unsafe { (*cell).assume_init_drop() });
        }
    }
}

/// The writing half: `offer` from one thread at a time.
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    /// The consumer's `head` as last seen, so that a producer only loads it when the
    /// queue looks full.
    head_cache: usize,
    /// The producer's own `tail`, which only it writes.
    tail: usize,
}

/// The reading half: `poll` from one thread at a time.
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    /// The producer's `tail` as last seen, so that a consumer only loads it when the
    /// queue looks empty.
    tail_cache: usize,
    /// The consumer's own `head`, which only it writes.
    head: usize,
}

/// A queue of `capacity` items, at least 1, as its two halves. The slots are allocated
/// here, once; nothing allocates after. A `capacity` of 0 is raised to 1, so that the
/// queue always takes an item an empty one is offered.
pub fn queue<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let capacity = capacity.max(1);
    let slots: Box<[UnsafeCell<MaybeUninit<T>>]> = (0..capacity)
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect();
    let shared = Arc::new(Shared {
        head: Padded(AtomicUsize::new(0)),
        tail: Padded(AtomicUsize::new(0)),
        slots,
    });
    (
        Producer {
            shared: Arc::clone(&shared),
            head_cache: 0,
            tail: 0,
        },
        Consumer {
            shared,
            tail_cache: 0,
            head: 0,
        },
    )
}

impl<T> Producer<T> {
    /// Puts `item` at the tail, or returns it when the queue is full: the caller keeps
    /// it and decides, as with every back-pressure in Cavalry (invariant 3). Never
    /// waits, never allocates.
    pub fn offer(&mut self, item: T) -> Result<(), T> {
        let capacity = self.shared.capacity();
        if self.tail.wrapping_sub(self.head_cache) >= capacity {
            self.head_cache = self.shared.head.0.load(Ordering::Acquire);
            if self.tail.wrapping_sub(self.head_cache) >= capacity {
                return Err(item);
            }
        }
        // SAFETY: `tail - head < capacity`, so the slot at `tail` was taken by the
        // consumer (it released `head` past the slot's last position, which the acquire
        // load above saw) or never written; only this producer writes slots (one per
        // queue, not `Clone`, and `&mut self` here), and the
        // consumer will read this one only after the release store of `tail` below.
        self.shared
            .slot(self.tail)
            .with_mut(|cell| unsafe { (*cell).write(item) });
        self.tail = self.tail.wrapping_add(1);
        self.shared.tail.0.store(self.tail, Ordering::Release);
        Ok(())
    }

    /// How many items the queue holds.
    pub fn capacity(&self) -> usize {
        self.shared.capacity()
    }

    /// Whether the other half has been dropped: the shared state has one owner left,
    /// this one. A conductor sweeps a registration whose subscription is gone this way,
    /// with no message between the threads (ADR 0021).
    pub fn peer_dropped(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }

    /// How many items are queued, a snapshot from a fresh load of the consumer's `head`:
    /// exact for the items this half put in, and possibly more than the queue holds by
    /// the time the caller reads it, since the consumer may take more.
    pub fn len(&self) -> usize {
        let head = self.shared.head.0.load(Ordering::Acquire);
        self.tail.wrapping_sub(head)
    }

    /// Whether nothing is queued, as this half last saw it.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Consumer<T> {
    /// Takes the item at the head, or `None` when the queue is empty. Never waits.
    pub fn poll(&mut self) -> Option<T> {
        if self.head == self.tail_cache {
            self.tail_cache = self.shared.tail.0.load(Ordering::Acquire);
            if self.head == self.tail_cache {
                return None;
            }
        }
        // SAFETY: `head < tail`, and the producer wrote the slot at `head` before the
        // release store of `tail` that the acquire load above saw; only this consumer
        // reads slots (one per queue, not `Clone`, and `&mut self` here), and the
        // producer will write this one again only after the
        // release store of `head` below.
        let item = self
            .shared
            .slot(self.head)
            .with(|cell| unsafe { (*cell).assume_init_read() });
        self.head = self.head.wrapping_add(1);
        self.shared.head.0.store(self.head, Ordering::Release);
        Some(item)
    }

    /// How many items the queue holds.
    pub fn capacity(&self) -> usize {
        self.shared.capacity()
    }

    /// Whether the other half has been dropped: the shared state has one owner left,
    /// this one. A conductor sweeps a registration whose subscription is gone this way,
    /// with no message between the threads (ADR 0021).
    pub fn peer_dropped(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }

    /// How many items are queued, a snapshot from a fresh load of the producer's `tail`:
    /// exact for the items this half has taken, and possibly fewer than the queue holds
    /// by the time the caller reads it, since the producer may put in more.
    pub fn len(&self) -> usize {
        let tail = self.shared.tail.0.load(Ordering::Acquire);
        tail.wrapping_sub(self.head)
    }

    /// Whether nothing is queued, as this half last saw it.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> core::fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("spsc::Producer")
            .field("capacity", &self.capacity())
            .field("tail", &self.tail)
            .finish()
    }
}

impl<T> core::fmt::Debug for Consumer<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("spsc::Consumer")
            .field("capacity", &self.capacity())
            .field("head", &self.head)
            .finish()
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};

    use super::*;

    #[test]
    fn a_full_queue_returns_the_item_and_an_empty_one_returns_none() {
        let (mut producer, mut consumer) = queue::<u32>(2);
        assert_eq!(producer.capacity(), 2);
        assert!(producer.is_empty() && consumer.is_empty());
        assert_eq!(consumer.poll(), None);
        assert_eq!(producer.offer(1), Ok(()));
        assert_eq!(producer.offer(2), Ok(()));
        assert_eq!(producer.offer(3), Err(3), "full: the item comes back");
        assert_eq!(producer.len(), 2);
        assert_eq!(consumer.len(), 2);
        assert_eq!(consumer.poll(), Some(1));
        assert_eq!(producer.offer(3), Ok(()), "a taken slot is written again");
        assert_eq!(consumer.poll(), Some(2));
        assert_eq!(consumer.poll(), Some(3));
        assert_eq!(consumer.poll(), None);
        assert!(consumer.is_empty());
    }

    #[test]
    fn a_capacity_of_zero_is_one_and_order_holds_across_wrapping() {
        let (mut producer, mut consumer) = queue::<usize>(0);
        assert_eq!(producer.capacity(), 1);
        for i in 0..1000 {
            assert_eq!(producer.offer(i), Ok(()));
            assert_eq!(producer.offer(i), Err(i));
            assert_eq!(consumer.poll(), Some(i));
            assert_eq!(consumer.poll(), None);
        }
    }

    #[test]
    fn items_left_in_the_queue_are_dropped_once_with_it() {
        static DROPS: StdAtomicUsize = StdAtomicUsize::new(0);
        struct Counted;
        impl Drop for Counted {
            fn drop(&mut self) {
                DROPS.fetch_add(1, StdOrdering::Relaxed);
            }
        }
        let (mut producer, mut consumer) = queue::<Counted>(4);
        for _ in 0..3 {
            assert!(producer.offer(Counted).is_ok());
        }
        drop(consumer.poll().expect("one item"));
        assert_eq!(
            DROPS.load(StdOrdering::Relaxed),
            1,
            "the taken item dropped by its owner"
        );
        drop(producer);
        assert_eq!(
            DROPS.load(StdOrdering::Relaxed),
            1,
            "one half gone: nothing dropped yet"
        );
        drop(consumer);
        assert_eq!(
            DROPS.load(StdOrdering::Relaxed),
            3,
            "the two left dropped with the queue"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)] // 100,000 items across threads: minutes under Miri; the loom model covers the protocol
    fn every_item_crosses_the_threads_once_and_in_order() {
        let (mut producer, mut consumer) = queue::<u64>(16);
        let total: u64 = 100_000;
        let consumer_thread = std::thread::spawn(move || {
            let mut next = 0;
            while next < total {
                match consumer.poll() {
                    Some(item) => {
                        assert_eq!(item, next);
                        next += 1;
                    }
                    None => std::hint::spin_loop(),
                }
            }
            assert_eq!(consumer.poll(), None);
        });
        let mut next = 0;
        while next < total {
            match producer.offer(next) {
                Ok(()) => next += 1,
                Err(back) => {
                    assert_eq!(back, next);
                    std::hint::spin_loop();
                }
            }
        }
        consumer_thread.join().expect("the consumer finished");
        assert!(producer.is_empty());
    }

    #[test]
    fn a_half_knows_when_its_peer_is_gone() {
        let (producer, consumer) = queue::<u8>(2);
        assert!(!producer.peer_dropped());
        assert!(!consumer.peer_dropped());
        drop(consumer);
        assert!(producer.peer_dropped());
        let (producer, consumer) = queue::<u8>(2);
        drop(producer);
        assert!(consumer.peer_dropped());
    }

    #[test]
    fn debug_names_the_halves() {
        let (producer, consumer) = queue::<u8>(3);
        assert!(format!("{producer:?}").contains("spsc::Producer"));
        assert!(format!("{consumer:?}").contains("capacity: 3"));
    }
}

#[cfg(loom)]
mod loom_tests {
    use loom::thread;

    use super::*;

    /// The producer offers three items through a queue of two, so that it meets the
    /// full case and the consumer the empty case; the consumer sees exactly the three,
    /// in order. loom explores every interleaving of the two threads' loads and stores
    /// and fails on a slot read that races a write, or a lost or duplicated item.
    #[test]
    fn three_items_through_two_slots_arrive_once_and_in_order() {
        loom::model(|| {
            let (mut producer, mut consumer) = queue::<u32>(2);
            let producer_thread = thread::spawn(move || {
                let mut next = 1;
                while next <= 3 {
                    match producer.offer(next) {
                        Ok(()) => next += 1,
                        Err(back) => {
                            assert_eq!(back, next);
                            thread::yield_now();
                        }
                    }
                }
            });
            let mut seen = Vec::new();
            while seen.len() < 3 {
                match consumer.poll() {
                    Some(item) => seen.push(item),
                    None => thread::yield_now(),
                }
            }
            assert_eq!(seen, vec![1, 2, 3]);
            assert_eq!(consumer.poll(), None);
            producer_thread.join().unwrap();
        });
    }

    /// An item left in the queue when both halves drop is dropped exactly once.
    #[test]
    fn an_item_left_behind_is_dropped_once() {
        loom::model(|| {
            let (mut producer, consumer) = queue::<Box<u32>>(1);
            let consumer_thread = thread::spawn(move || drop(consumer));
            assert!(producer.offer(Box::new(7)).is_ok());
            drop(producer);
            consumer_thread.join().unwrap();
        });
    }
}
