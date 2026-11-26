//! What gets a client's conductor back when its park outlived its timeout (ADR 0177),
//! as a driver's watch does for the driver's five threads (ADR 0176). On some machines
//! a timed wait is, once in tens of millions, never ended by its timer (ADR 0171;
//! `docs/how-to/find-a-wait-that-does-not-return.md`). A conductor that is held sends
//! no keepalive, and its driver times the client out after ten seconds.
//!
//! The conductor stores, before it parks, the time its park is due to end, and clears
//! it when the park has returned: two stores on its idle path. A second thread of the
//! client, the watch, parks for [`WATCH_PERIOD`] at a time and unparks a conductor
//! whose park is more than [`OVERDUE`] past due. The watch's own park can be lost as
//! any other can, so it stores its due time too, and the conductor looks at it on each
//! idle pass and unparks the watch the same way. Each counts a park once, by its due
//! time, in [`Pair::parks_overdue`], which the conductor publishes in a counter of the
//! client's own, `client.conductor_parks_overdue`.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::thread::Thread;
use std::time::Duration;

/// How long the watch parks between two looks at the conductor.
pub(crate) const WATCH_PERIOD: Duration = Duration::from_millis(100);

/// How far past its due time a park may be before it is ended: the driver's figure,
/// set against how late a sound park was seen to get (ADR 0176).
pub(crate) const OVERDUE: Duration = Duration::from_millis(200);

/// Not parked.
const NOT_PARKED: i64 = 0;

/// When one thread's park is due to end, on the nano clock, or that it is not parked.
/// One writer, the thread itself, on its idle path; read by the other thread of the
/// pair. A slot of its own (ADR 0011).
#[derive(Debug)]
#[repr(C, align(128))]
pub(crate) struct WakeDue(AtomicI64);

impl WakeDue {
    const fn new() -> Self {
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
    /// [`OVERDUE`] before `now`: the park's own name, by which it is counted once.
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

/// What a client's conductor and its watch share.
#[derive(Debug)]
pub(crate) struct Pair {
    /// When the conductor's park is due.
    pub(crate) conductor: WakeDue,
    /// When the watch's park is due.
    pub(crate) watch: WakeDue,
    /// The conductor's thread, known once it has started.
    pub(crate) conductor_thread: OnceLock<Thread>,
    /// The watch's thread, known once it has started.
    pub(crate) watch_thread: OnceLock<Thread>,
    /// The parks of either that the other ended, each counted once. Written by both,
    /// with an atomic add; published by the conductor alone.
    pub(crate) parks_overdue: AtomicU64,
}

impl Pair {
    pub(crate) const fn new() -> Self {
        Self {
            conductor: WakeDue::new(),
            watch: WakeDue::new(),
            conductor_thread: OnceLock::new(),
            watch_thread: OnceLock::new(),
            parks_overdue: AtomicU64::new(0),
        }
    }

    /// The conductor's look at the watch, on an idle pass: when the watch's park is
    /// overdue it is counted, if `counted` does not already hold its due time, and the
    /// watch is unparked. One load and one compare when it is not.
    pub(crate) fn conductor_looks(&self, now: i64, counted: &mut i64) {
        self.look(&self.watch, &self.watch_thread, now, counted);
    }

    /// The watch's look at the conductor, after each of its parks.
    fn watch_looks(&self, now: i64, counted: &mut i64) {
        self.look(&self.conductor, &self.conductor_thread, now, counted);
    }

    fn look(&self, due: &WakeDue, thread: &OnceLock<Thread>, now: i64, counted: &mut i64) {
        let Some(due) = due.overdue(now) else {
            return;
        };
        let Some(thread) = thread.get() else {
            return;
        };
        // Counted before the thread is woken, so that the count never lags the thread
        // it explains; and once, however many looks the park takes to end.
        if *counted != due {
            *counted = due;
            self.parks_overdue.fetch_add(1, Ordering::AcqRel);
        }
        thread.unpark();
    }

    /// The watch's loop, until `closed`: a park of a period, then a look at the
    /// conductor. `first_park` is for a test: the watch's first park lasts that long in
    /// place of a period, which stands for a park whose timeout never comes.
    pub(crate) fn watch(
        &self,
        now: impl Fn() -> i64,
        closed: impl Fn() -> bool,
        mut first_park: Option<Duration>,
    ) {
        let mut counted = NOT_PARKED;
        while !closed() {
            let park = first_park.take().unwrap_or(WATCH_PERIOD);
            self.watch.parking(now(), WATCH_PERIOD);
            std::thread::park_timeout(park);
            self.watch.woke();
            self.watch_looks(now(), &mut counted);
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
        assert_eq!(due.overdue(at + grace), None, "at the grace");
        assert_eq!(due.overdue(at + grace + 1), Some(at), "past the grace");
        due.woke();
        assert_eq!(due.overdue(i64::MAX), None, "woken");
    }

    /// A park that is seen overdue at many looks is counted once, and unparked at each.
    #[test]
    fn an_overdue_park_is_counted_once_however_many_looks_it_takes() {
        let pair = Pair::new();
        let _ = pair.watch_thread.set(std::thread::current());
        pair.watch.parking(1_000, Duration::from_millis(100));
        let late = 1_000 + 100_000_000 + i64::try_from(OVERDUE.as_nanos()).expect("a grace") + 1;
        let mut counted = 0;
        for look in 0..50 {
            pair.conductor_looks(late + look, &mut counted);
        }
        assert_eq!(pair.parks_overdue.load(Ordering::Acquire), 1);
        // The next park of the watch is another park.
        pair.watch.woke();
        pair.watch.parking(late, Duration::from_millis(100));
        pair.conductor_looks(late * 2, &mut counted);
        assert_eq!(pair.parks_overdue.load(Ordering::Acquire), 2);
        // The unparks left this thread a token, which one park takes at once.
        std::thread::park_timeout(Duration::from_secs(3600));
    }
}
