//! A member's timers: the alarm clock of the service beside it (`spec/cluster.md`,
//! "The service", "Timers", "The member's timers" and "The leader's timer events";
//! ADR 0151). In Aeron, this is the consensus module's timer service.
//!
//! A service tells the member on its driver of each timer its program scheduled, with
//! a schedule timer, and of each that is pending no more, with a cancel timer. The
//! member keeps what it was told, and while it leads it appends a timer event for a
//! timer whose deadline its clock has reached. It keeps that timer, marked as one
//! whose event it appended, so that it appends no second event for it. A schedule
//! timer for the id takes the mark off, and a cancel timer removes the timer. A
//! service sends every pending timer again each timer refresh interval, so a mark
//! on a timer that is pending still, as after an event that a later leader's log
//! does not hold, is off within that interval.
//!
//! What a member holds here is no state it computes from the log. It is in no
//! snapshot, a member that starts holds none, and nothing of the cluster's safety
//! rests on it: which timer event is handed to a program is decided by every service
//! from the log alone ([`cavalry_proto::cluster::ServiceTimers`]). A table that is
//! behind, ahead or empty costs the log an entry that every service passes over, or
//! a timer its lateness, and never a timer handed over twice.
//!
//! Its bound is [`MAX_MEMBER_TIMERS`], allocated when the table is made. A schedule
//! of another id at the bound takes the place of the timer whose event the member
//! appended longest ago. With no such timer the member holds more than its services
//! can have pending, so some of what it holds is pending nowhere: it removes every
//! timer and takes the new one, and the service beside it sends the pending ones
//! again within a timer refresh interval.

use cavalry_proto::cluster::MAX_MEMBER_TIMERS;

/// No position: a timer that has no mark.
const NONE: i64 = -1;

/// What [`MemberTimers::schedule`] did with a schedule timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheduled {
    /// The member holds the timer with the deadline given.
    Held,
    /// The member holds it in the place of the timer whose event it appended longest
    /// ago, its table being at its bound, which `cluster.timers_replaced` counts.
    Replaced,
    /// The table was at its bound with no timer in it whose event the member had
    /// appended: the member removed every timer and holds this one alone, which
    /// `cluster.timers_cleared` counts. The service beside it sends every pending
    /// timer again within a timer refresh interval.
    Cleared,
}

/// The timers a member holds, in the rising order of their correlation ids.
#[derive(Debug)]
pub struct MemberTimers {
    ids: Box<[i64]>,
    deadlines: Box<[i64]>,
    // The position of the entry of the timer's event that this member appended, or
    // NONE: the mark, which `doctor` and a test can tell an event's place by.
    appended_at: Box<[i64]>,
    // The count of events appended when this one was: the lowest is the oldest.
    appended_order: Box<[u64]>,
    len: usize,
    events: u64,
    // The earliest deadline of a timer with no mark, or i64::MAX when there is none;
    // `None` when a change has made it stale.
    earliest: Option<i64>,
}

impl Default for MemberTimers {
    fn default() -> Self {
        Self::new()
    }
}

impl MemberTimers {
    /// A table with no timer, for [`MAX_MEMBER_TIMERS`].
    pub fn new() -> Self {
        MemberTimers {
            ids: vec![0; MAX_MEMBER_TIMERS].into_boxed_slice(),
            deadlines: vec![0; MAX_MEMBER_TIMERS].into_boxed_slice(),
            appended_at: vec![NONE; MAX_MEMBER_TIMERS].into_boxed_slice(),
            appended_order: vec![0; MAX_MEMBER_TIMERS].into_boxed_slice(),
            len: 0,
            events: 0,
            earliest: Some(i64::MAX),
        }
    }

    /// How many timers the member holds: what `cluster.timers` holds.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the member holds no timer.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn find(&self, correlation_id: i64) -> Result<usize, usize> {
        self.ids
            .get(..self.len)
            .unwrap_or(&[])
            .binary_search(&correlation_id)
    }

    fn remove(&mut self, index: usize) {
        if index >= self.len {
            return;
        }
        self.ids.copy_within(index + 1..self.len, index);
        self.deadlines.copy_within(index + 1..self.len, index);
        self.appended_at.copy_within(index + 1..self.len, index);
        self.appended_order.copy_within(index + 1..self.len, index);
        self.len -= 1;
        self.earliest = None;
    }

    /// What the member holds of the timer `correlation_id`: its deadline, and the
    /// position of the entry of the event the member appended for it, when it is
    /// marked. `None` for a timer the member does not hold.
    pub fn get(&self, correlation_id: i64) -> Option<(i64, Option<i64>)> {
        let index = self.find(correlation_id).ok()?;
        let mark = self
            .appended_at
            .get(index)
            .copied()
            .filter(|mark| *mark != NONE);
        Some((*self.deadlines.get(index)?, mark))
    }

    /// Takes a schedule timer: the member holds the timer `correlation_id` with
    /// `deadline`, and with no mark, whether it held the id or not.
    pub fn schedule(&mut self, correlation_id: i64, deadline: i64) -> Scheduled {
        let mut outcome = Scheduled::Held;
        let index = match self.find(correlation_id) {
            Ok(index) => index,
            Err(mut index) => {
                if self.len >= MAX_MEMBER_TIMERS {
                    let oldest = (0..self.len)
                        .filter(|at| self.appended_at.get(*at).is_some_and(|mark| *mark != NONE))
                        .min_by_key(|at| self.appended_order.get(*at).copied().unwrap_or(u64::MAX));
                    match oldest {
                        Some(oldest) => {
                            self.remove(oldest);
                            if oldest < index {
                                index -= 1;
                            }
                            outcome = Scheduled::Replaced;
                        }
                        None => {
                            self.clear();
                            index = 0;
                            outcome = Scheduled::Cleared;
                        }
                    }
                }
                self.ids.copy_within(index..self.len, index + 1);
                self.deadlines.copy_within(index..self.len, index + 1);
                self.appended_at.copy_within(index..self.len, index + 1);
                self.appended_order.copy_within(index..self.len, index + 1);
                self.len += 1;
                if let Some(id) = self.ids.get_mut(index) {
                    *id = correlation_id;
                }
                index
            }
        };
        if let Some(slot) = self.deadlines.get_mut(index) {
            *slot = deadline;
        }
        if let Some(mark) = self.appended_at.get_mut(index) {
            *mark = NONE;
        }
        self.earliest = None;
        outcome
    }

    /// Takes a cancel timer: the member holds the timer `correlation_id` no more,
    /// with its mark. `true` when it held it.
    pub fn cancel(&mut self, correlation_id: i64) -> bool {
        match self.find(correlation_id) {
            Ok(index) => {
                self.remove(index);
                true
            }
            Err(_) => false,
        }
    }

    /// The timer a leader appends an event for next, at the cluster's time `now_ms`:
    /// the one with the earliest deadline at or before it among those with no mark,
    /// and of two with one deadline the one of the lower id. `None` when no such
    /// timer is due, which costs one compare while nothing has changed since the
    /// last call.
    pub fn due(&mut self, now_ms: i64) -> Option<i64> {
        let earliest = match self.earliest {
            Some(earliest) => earliest,
            None => {
                let earliest = (0..self.len)
                    .filter(|at| self.appended_at.get(*at) == Some(&NONE))
                    .filter_map(|at| self.deadlines.get(at).copied())
                    .min()
                    .unwrap_or(i64::MAX);
                self.earliest = Some(earliest);
                earliest
            }
        };
        if earliest > now_ms || earliest == i64::MAX && self.len == 0 {
            return None;
        }
        (0..self.len)
            .filter(|at| self.appended_at.get(*at) == Some(&NONE))
            .find(|at| self.deadlines.get(*at) == Some(&earliest))
            .and_then(|at| self.ids.get(at).copied())
    }

    /// Marks the timer `correlation_id` with `position`, where the entry of the event
    /// the member appended for it begins: the member appends no other for it while
    /// the mark stays.
    pub fn appended(&mut self, correlation_id: i64, position: i64) {
        if let Ok(index) = self.find(correlation_id) {
            if let Some(mark) = self.appended_at.get_mut(index) {
                *mark = position;
            }
            if let Some(order) = self.appended_order.get_mut(index) {
                *order = self.events;
            }
            self.events = self.events.wrapping_add(1);
            self.earliest = None;
        }
    }

    /// The timers the member holds, each a correlation id, a deadline and whether it
    /// is marked, in the rising order of the ids.
    pub fn iter(&self) -> impl Iterator<Item = (i64, i64, bool)> + '_ {
        (0..self.len).filter_map(|at| {
            Some((
                *self.ids.get(at)?,
                *self.deadlines.get(at)?,
                *self.appended_at.get(at)? != NONE,
            ))
        })
    }

    /// Removes every timer: a member that starts holds none.
    pub fn clear(&mut self) {
        self.len = 0;
        self.earliest = Some(i64::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timer_is_due_once_and_again_when_it_is_scheduled_again() {
        let mut timers = MemberTimers::new();
        assert_eq!(timers.due(i64::MAX), None, "an empty table has nothing due");
        assert_eq!(timers.schedule(7, 100), Scheduled::Held);
        assert_eq!(timers.schedule(3, 100), Scheduled::Held);
        assert_eq!(timers.schedule(9, 50), Scheduled::Held);
        assert_eq!(timers.due(49), None);
        // Earliest deadline first, and the lower id of two with one deadline.
        assert_eq!(timers.due(100), Some(9));
        timers.appended(9, 320);
        assert_eq!(timers.due(100), Some(3));
        timers.appended(3, 352);
        assert_eq!(timers.due(100), Some(7));
        timers.appended(7, 384);
        assert_eq!(timers.due(i64::MAX), None, "each has its event");
        assert_eq!(
            timers.len(),
            3,
            "a timer stays, marked, until its service says"
        );
        // A schedule timer takes the mark off; a cancel timer removes the timer.
        assert_eq!(timers.schedule(9, 500), Scheduled::Held);
        assert_eq!(timers.due(499), None);
        assert_eq!(timers.due(500), Some(9));
        assert!(timers.cancel(9));
        assert!(!timers.cancel(9));
        assert_eq!(timers.due(i64::MAX), None);
        assert_eq!(timers.len(), 2);
        timers.clear();
        assert!(timers.is_empty());
        assert_eq!(timers.due(i64::MAX), None);
    }

    #[test]
    fn at_its_bound_a_schedule_replaces_the_oldest_marked_timer_or_clears_the_table() {
        let mut timers = MemberTimers::new();
        let count = i64::try_from(MAX_MEMBER_TIMERS).expect("a count");
        for id in 0..count {
            assert_eq!(timers.schedule(id, 1_000 + id), Scheduled::Held, "{id}");
        }
        assert_eq!(timers.len(), MAX_MEMBER_TIMERS);
        assert_eq!(timers.due(1_000), Some(0));
        // A held id is scheduled at any count.
        assert_eq!(timers.schedule(4, 9_999_999), Scheduled::Held);
        // Two with marks: the one appended first gives its place, then the other.
        timers.appended(10, 64);
        timers.appended(2, 96);
        assert_eq!(timers.schedule(count, 5), Scheduled::Replaced);
        assert!(!timers.cancel(10), "the oldest marked timer gave its place");
        assert_eq!(timers.schedule(-1, 6), Scheduled::Replaced);
        assert!(!timers.cancel(2));
        assert_eq!(timers.len(), MAX_MEMBER_TIMERS);
        // The table is in the order of the ids still: the two new ones are due first.
        assert_eq!(timers.due(5), Some(count));
        timers.appended(count, 128);
        assert_eq!(timers.due(6), Some(-1));
        // A schedule timer for a marked id takes its mark off, the same deadline
        // included: a service's refresh says the timer is pending still.
        assert_eq!(timers.schedule(count, 5), Scheduled::Held);
        assert_eq!(timers.due(5), Some(count));
        assert_eq!(timers.iter().filter(|(_, _, marked)| *marked).count(), 0);
        // No timer has a mark and the table is full: another id clears it, and is
        // the one timer the member then holds.
        assert_eq!(timers.schedule(-2, 7), Scheduled::Cleared);
        assert_eq!(timers.len(), 1);
        assert_eq!(timers.get(-2), Some((7, None)));
        assert_eq!(timers.due(7), Some(-2));
    }
}
