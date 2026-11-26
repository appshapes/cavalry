//! What gets a thread back whose park outlived its timeout (ADR 0176). On some machines
//! a timed wait is, once in tens of millions, never ended by its timer, and the thread
//! then does no work until something wakes it (ADR 0171;
//! `docs/how-to/find-a-wait-that-does-not-return.md`). A stop wakes every thread; this
//! is for the time before a stop.
//!
//! Each of the driver's five threads stores, before it parks, the time its park is due
//! to end, and clears it when the park has returned: two stores on the idle path, and
//! nothing where a message is handled. A sixth thread, the watch, parks for
//! [`WATCH_PERIOD`] at a time and unparks any of the five whose park is more than
//! [`OVERDUE`] past due, counting each in `driver.parks_overdue`. The watch's own park
//! can be lost as any other can, so it stores its due time too, and the conductor looks
//! at it on each idle pass and unparks the watch the same way. A lost wake-up then
//! costs its thread a stall of at most the period and the grace, 300 ms past its
//! timeout, while the watch's own park is on time. Only the conductor's and the
//! watch's lost together are not ended: the product of two rates of one in tens of
//! millions.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::Thread;
use std::time::Duration;

use cavalry_shm::counters::Counter;

use crate::conductor::now_nanos;

/// How long the watch parks between two looks at the five threads.
pub(crate) const WATCH_PERIOD: Duration = Duration::from_millis(100);

/// How far past its due time a park may be before the watch ends it: above what a
/// loaded host was seen to add to the wake-up of a thread that is only late, 53 ms in
/// 1.8 billion waits (ADR 0176), so that a count here is a park that was not coming
/// back by itself, or a host that could not run the thread for that long.
pub(crate) const OVERDUE: Duration = Duration::from_millis(200);

/// Not parked.
const NOT_PARKED: i64 = 0;

/// When one thread's park is due to end, on the nano clock, or that it is not parked.
/// One writer, the thread itself, on its idle path; read by the watch once a period.
/// A slot of its own, since the writer's stores must not share a line with another
/// thread's (ADR 0011).
#[derive(Debug)]
#[repr(C, align(128))]
pub(crate) struct WakeDue(AtomicI64);

impl WakeDue {
    /// A thread that is not parked.
    pub(crate) const fn new() -> Self {
        Self(AtomicI64::new(NOT_PARKED))
    }

    /// The thread is about to park for `park` at most, at `now` on the nano clock.
    pub(crate) fn parking(&self, now: i64, park: Duration) {
        let due = now.saturating_add(i64::try_from(park.as_nanos()).unwrap_or(i64::MAX));
        // Never the value that means not parked.
        self.0.store(due.max(1), Ordering::Release);
    }

    /// The park has returned.
    pub(crate) fn woke(&self) {
        self.0.store(NOT_PARKED, Ordering::Release);
    }

    /// The due time of the thread's park, when it is parked and that time is more than
    /// [`OVERDUE`] before `now`: the park's own name, by which whoever ends it counts
    /// it once, however many looks it takes to end.
    pub(crate) fn overdue(&self, now: i64) -> Option<i64> {
        let due = self.0.load(Ordering::Acquire);
        (due != NOT_PARKED
            && now.saturating_sub(due) > i64::try_from(OVERDUE.as_nanos()).unwrap_or(i64::MAX))
        .then_some(due)
    }

    /// What the slot holds, for a test: a due time, or 0 for not parked.
    #[cfg(test)]
    pub(crate) fn raw(&self) -> i64 {
        self.0.load(Ordering::Acquire)
    }
}

impl Default for WakeDue {
    fn default() -> Self {
        Self::new()
    }
}

/// The watch as the conductor sees it: when its park is due, and its thread, which is
/// known only once the watch has started, after the conductor.
#[derive(Debug, Default)]
pub(crate) struct Watched {
    pub(crate) due: WakeDue,
    pub(crate) thread: OnceLock<Thread>,
}

impl Watched {
    /// Unparks the watch when its park is overdue, and returns that park's due time.
    /// The conductor calls it on an idle pass, one load and one compare, and may
    /// call it many times before the watch has woken and cleared its due time: it
    /// counts the park when the due time is one it has not counted.
    pub(crate) fn rescue(&self, now: i64) -> Option<i64> {
        let due = self.due.overdue(now)?;
        let thread = self.thread.get()?;
        thread.unpark();
        Some(due)
    }
}

/// The watch's own state: the five threads with their due times, its own due time as
/// the conductor reads it, and the counter.
pub(crate) struct Watch {
    pub(crate) threads: Vec<(Thread, Arc<WakeDue>)>,
    pub(crate) own: Arc<Watched>,
    pub(crate) parks_overdue: Counter,
    /// For a test: the watch's first park lasts this long in place of a period, which
    /// stands for a park whose timeout never comes, so that the test passes only when
    /// the conductor unparks the watch.
    #[cfg(test)]
    pub(crate) first_park: Option<Duration>,
}

impl Watch {
    /// The watch's loop, until `stop`: a park of a period, then a look at each thread.
    pub(crate) fn run(self, stop: &AtomicBool) {
        #[cfg(test)]
        let mut first_park = self.first_park;
        // The due time of the last park counted for each thread: a thread that has
        // been unparked and has not yet cleared its due time is unparked again at the
        // next look, and counted once.
        let mut counted = vec![NOT_PARKED; self.threads.len()];
        while !stop.load(Ordering::Acquire) {
            let park = WATCH_PERIOD;
            #[cfg(test)]
            let park = first_park.take().unwrap_or(park);
            self.own.due.parking(now_nanos(), WATCH_PERIOD);
            std::thread::park_timeout(park);
            self.own.due.woke();
            let now = now_nanos();
            for ((thread, due), counted) in self.threads.iter().zip(&mut counted) {
                if let Some(due) = due.overdue(now) {
                    // The thread reads its work and its stop flag as it wakes. A thread
                    // that woke by itself between the load and this leaves a token, and
                    // its next park returns at once: one duty cycle.
                    thread.unpark();
                    if *counted != due {
                        *counted = due;
                        self.parks_overdue.increment();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_park_is_overdue_only_past_its_due_time_and_the_grace_and_only_while_parked() {
        let due = WakeDue::new();
        let grace = i64::try_from(OVERDUE.as_nanos()).expect("a grace in nanoseconds");
        assert_eq!(due.overdue(i64::MAX), None, "not parked");
        due.parking(1_000, Duration::from_millis(1));
        let at = 1_000 + 1_000_000;
        assert_eq!(due.overdue(at), None, "due now");
        assert_eq!(due.overdue(at + grace), None, "at the grace");
        assert_eq!(due.overdue(at + grace + 1), Some(at), "past the grace");
        due.woke();
        assert_eq!(due.overdue(i64::MAX), None, "woken");
        // A park at the clock's start is still a park.
        due.parking(0, Duration::ZERO);
        assert_eq!(due.overdue(grace + 2), Some(1));
    }
}
