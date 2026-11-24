//! A service's timers against a second reading of the rule (`spec/cluster.md`, "The
//! service", "Timers"; ADR 0151): [`ServiceTimers`] and a model of a few lines over
//! a map take the same seeded calls, and after each the two hold the same timers
//! and have the same thing to tell the member next. The spec's `timers` vectors
//! (`tests/cluster_conformance.rs`) are sequences somebody chose; these are ones
//! nobody did, at the bound of the table and at the bound of the cancels it holds.
//! And the edges of the calls that no vector reaches: a snapshot's timers out of
//! order and past the bound, a handling begun inside another, and a cleared table.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use cavalry_proto::cluster::{
    MAX_SERVICE_TIMERS, MAX_UNSENT_TIMER_CANCELS, ServiceTimers, TimerEventIs, TimerOutOfOrder,
    TimerWord, TimersFull,
};

/// The rule read a second time, with nothing of [`ServiceTimers`] in it.
#[derive(Default)]
struct Model {
    pending: BTreeMap<i64, i64>,
    unsent: BTreeSet<i64>,
    cancels: VecDeque<i64>,
    held: bool,
    before: BTreeMap<i64, i64>,
    /// The timer event being handled, and whether it was handed over.
    event: Option<(i64, bool)>,
    stays: bool,
    /// Whether a handling is open, and whether a refresh came during it.
    open: bool,
    again: bool,
}

impl Model {
    fn begin(&mut self, event: Option<(i64, i64)>) -> Option<bool> {
        self.before = self.pending.clone();
        self.stays = false;
        self.open = true;
        self.event = event.map(|(id, timestamp)| {
            let handed = self
                .pending
                .get(&id)
                .is_some_and(|deadline| *deadline <= timestamp);
            (id, handed)
        });
        self.event.map(|(_, handed)| handed)
    }

    fn handed(&self) -> Option<i64> {
        self.event.filter(|(_, handed)| *handed).map(|(id, _)| id)
    }

    fn schedule(&mut self, id: i64, deadline: i64) -> bool {
        if !self.pending.contains_key(&id) && self.pending.len() >= MAX_SERVICE_TIMERS {
            return false;
        }
        self.pending.insert(id, deadline);
        if self.handed() == Some(id) {
            self.stays = true;
        }
        true
    }

    fn cancel(&mut self, id: i64) {
        if self.handed() == Some(id) {
            self.stays = false;
        } else {
            self.pending.remove(&id);
        }
    }

    fn hold_cancel(&mut self, id: i64) -> usize {
        if self.held {
            return 0;
        }
        let mut dropped = 0;
        if self.cancels.len() >= MAX_UNSENT_TIMER_CANCELS {
            self.cancels.pop_front();
            dropped = 1;
        }
        self.cancels.push_back(id);
        dropped
    }

    /// A refresh: every pending timer is to be sent again, when the handling that
    /// is open ends, and not at all while the service reads history.
    fn refresh(&mut self) {
        if self.held {
            return;
        }
        if self.open {
            self.again = true;
        } else {
            self.unsent = self.pending.keys().copied().collect();
        }
    }

    fn end(&mut self, taken: bool) -> usize {
        let dropped = self.end_timers(taken);
        self.open = false;
        if std::mem::take(&mut self.again) {
            self.refresh();
        }
        dropped
    }

    fn end_timers(&mut self, taken: bool) -> usize {
        let event = self.event.take();
        if !taken {
            self.pending = std::mem::take(&mut self.before);
            return 0;
        }
        let mut dropped = 0;
        match event {
            Some((id, true)) if !self.stays => {
                self.pending.remove(&id);
            }
            Some((id, true)) => {
                self.unsent.insert(id);
            }
            // An event that is passed over changes nothing and tells nothing.
            Some((_, false)) | None => {}
        }
        let before = std::mem::take(&mut self.before);
        for id in before.keys() {
            if !self.pending.contains_key(id) {
                dropped += self.hold_cancel(*id);
            }
        }
        for (id, deadline) in &self.pending {
            if before.get(id) != Some(deadline) {
                self.unsent.insert(*id);
            }
        }
        self.unsent.retain(|id| self.pending.contains_key(id));
        dropped
    }

    fn next_word(&self) -> Option<TimerWord> {
        if self.held || self.open {
            return None;
        }
        if let Some(&correlation_id) = self.cancels.front() {
            return Some(TimerWord::Cancel { correlation_id });
        }
        let correlation_id = *self.unsent.first()?;
        Some(TimerWord::Schedule {
            correlation_id,
            deadline: self.pending[&correlation_id],
        })
    }

    fn told(&mut self, word: TimerWord) {
        match word {
            TimerWord::Cancel { correlation_id } => {
                if self.cancels.front() == Some(&correlation_id) {
                    self.cancels.pop_front();
                }
            }
            TimerWord::Schedule {
                correlation_id,
                deadline,
            } => {
                if self.pending.get(&correlation_id) == Some(&deadline) {
                    self.unsent.remove(&correlation_id);
                }
            }
        }
    }
}

struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn same(timers: &ServiceTimers, model: &Model, at: &str) {
    let held: Vec<(i64, i64)> = timers.iter().collect();
    let expected: Vec<(i64, i64)> = model.pending.iter().map(|(id, at)| (*id, *at)).collect();
    assert_eq!(held, expected, "{at}: the pending timers");
    assert_eq!(timers.len(), expected.len(), "{at}: how many");
    assert_eq!(timers.next_word(), model.next_word(), "{at}: the next word");
}

/// One seed: `steps` handlings over ids from `low` to `low + span`, the table
/// holding `filled` timers below them first.
fn run(seed: u64, filled: i64, low: i64, span: u64, steps: usize) -> (usize, usize, usize) {
    let mut draw = Draw(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut timers = Box::new(ServiceTimers::new());
    let mut model = Model::default();
    let (mut refused, mut dropped_all, mut not_taken) = (0, 0, 0);
    for id in 0..filled {
        timers.begin_entry();
        model.begin(None);
        assert_eq!(timers.schedule(id, id).is_ok(), model.schedule(id, id));
        assert_eq!(timers.end_entry(true), model.end(true));
    }
    let id = |draw: &mut Draw| low + i64::try_from(draw.below(span)).expect("an id");
    for step in 0..steps {
        let at = format!("seed {seed}, step {step}");
        // An entry, or a timer event whose time is near the deadlines the calls draw.
        let handed = if draw.below(3) == 0 {
            let event = (
                id(&mut draw),
                i64::try_from(draw.below(40)).expect("a time"),
            );
            let is = timers.begin_event(event.0, event.1);
            let handed = model.begin(Some(event)).expect("an event");
            assert_eq!(is == TimerEventIs::Handed, handed, "{at}: handed");
            if let TimerEventIs::Early(deadline) = is {
                assert_eq!(model.pending.get(&event.0), Some(&deadline), "{at}: early");
            }
            Some(handed)
        } else {
            timers.begin_entry();
            model.begin(None);
            None
        };
        let mut taken = true;
        if handed != Some(false) {
            for _ in 0..draw.below(5) {
                let id = id(&mut draw);
                if draw.below(3) == 0 {
                    timers.cancel(id);
                    model.cancel(id);
                } else {
                    let deadline = i64::try_from(draw.below(40)).expect("a time");
                    let took = timers.schedule(id, deadline);
                    assert_eq!(took.is_ok(), model.schedule(id, deadline), "{at}: refused");
                    refused += usize::from(took == Err(TimersFull));
                }
                // Nothing of a handling that has not ended is told.
                assert_eq!(timers.next_word(), None, "{at}: told inside a handling");
                // A refresh that comes while a handler runs waits for its end.
                if draw.below(12) == 0 {
                    timers.tell_all_again();
                    model.refresh();
                }
            }
            taken = draw.below(4) != 0;
        }
        not_taken += usize::from(!taken);
        let dropped = timers.end_entry(taken);
        assert_eq!(dropped, model.end(taken), "{at}: cancels dropped");
        dropped_all += dropped;
        same(&timers, &model, &at);
        match draw.below(40) {
            0 => {
                timers.hold();
                model.held = true;
                model.cancels.clear();
            }
            1 => {
                timers.release();
                model.held = false;
                model.refresh();
            }
            2..=4 => {
                timers.tell_all_again();
                model.refresh();
            }
            _ => {}
        }
        // The stream takes a few of the words, or none.
        for _ in 0..draw.below(4) {
            let Some(word) = timers.next_word() else {
                break;
            };
            timers.told(word);
            model.told(word);
        }
        same(&timers, &model, &at);
    }
    (refused, dropped_all, not_taken)
}

#[test]
fn a_services_timers_are_the_rules_under_calls_nobody_chose() {
    let (mut refused, mut dropped, mut not_taken) = (0, 0, 0);
    for seed in 1..=8 {
        // A few ids, far from every bound; then the table one short of its bound,
        // with ids that are in it and ids that are not.
        let few = run(seed, 0, -3, 9, 4_000);
        let full = run(
            seed,
            i64::try_from(MAX_SERVICE_TIMERS).expect("the bound") - 1,
            i64::try_from(MAX_SERVICE_TIMERS).expect("the bound") - 5,
            9,
            600,
        );
        refused += few.0 + full.0;
        dropped += few.1 + full.1;
        not_taken += few.2 + full.2;
    }
    assert!(refused > 0, "no schedule was refused at the bound");
    assert!(not_taken > 0, "every handling was taken");
    // 1,023 timers and a stream that takes at most three words after a handling
    // hold cancels to their bound only when the table is emptied: below.
    let _ = dropped;
}

#[test]
fn the_cancels_a_service_holds_are_bounded_and_the_oldest_is_dropped() {
    let mut timers = Box::new(ServiceTimers::new());
    let bound = i64::try_from(MAX_UNSENT_TIMER_CANCELS).expect("the bound");
    // Fill the table and let the stream take every schedule.
    for id in 0..bound {
        timers.begin_entry();
        timers.schedule(id, 0).expect("room");
        assert_eq!(timers.end_entry(true), 0);
    }
    while let Some(word) = timers.next_word() {
        timers.told(word);
    }
    // One entry cancels all of them: 1,024 cancels held, none dropped.
    timers.begin_entry();
    for id in 0..bound {
        timers.cancel(id);
    }
    assert_eq!(timers.end_entry(true), 0);
    assert!(timers.is_empty());
    // Two more, with the stream taking nothing: the two oldest are dropped.
    for id in [bound, bound + 1] {
        timers.begin_entry();
        timers.schedule(id, 0).expect("room");
        assert_eq!(timers.end_entry(true), 0);
        timers.begin_entry();
        timers.cancel(id);
        assert_eq!(
            timers.end_entry(true),
            1,
            "the oldest cancel is dropped for {id}"
        );
    }
    let mut told = Vec::new();
    while let Some(word) = timers.next_word() {
        timers.told(word);
        told.push(word);
    }
    let expected: Vec<TimerWord> = (2..bound + 2)
        .map(|correlation_id| TimerWord::Cancel { correlation_id })
        .collect();
    assert_eq!(told, expected);
}

#[test]
fn a_snapshots_timers_are_taken_in_rising_order_up_to_the_bound() {
    let mut timers = Box::new(ServiceTimers::new());
    timers.hold();
    assert_eq!(timers.load(-5, 100), Ok(()));
    assert_eq!(
        timers.load(-5, 100),
        Err(TimerOutOfOrder),
        "the same id twice"
    );
    assert_eq!(
        timers.load(-6, 100),
        Err(TimerOutOfOrder),
        "an id below the last"
    );
    assert_eq!(timers.load(i64::MAX, -1), Ok(()));
    assert_eq!(timers.load(i64::MAX, -1), Err(TimerOutOfOrder));
    assert_eq!(
        timers.iter().collect::<Vec<_>>(),
        [(-5, 100), (i64::MAX, -1)]
    );
    // Nothing is told while the service reads history, a refresh changes that not,
    // and all of it is told after.
    assert_eq!(timers.next_word(), None);
    timers.tell_all_again();
    assert_eq!(timers.next_word(), None, "a refresh ends no history");
    timers.release();
    assert_eq!(
        timers.next_word(),
        Some(TimerWord::Schedule {
            correlation_id: -5,
            deadline: 100
        })
    );
    // A cleared table holds nothing and has nothing to tell.
    timers.clear();
    assert!(timers.is_empty());
    assert_eq!(timers.next_word(), None);
    assert_eq!(timers.deadline(-5), None);
    // A timer taken from a snapshot is one the member has not been sent, with no
    // refresh to say so.
    assert_eq!(timers.load(3, 30), Ok(()));
    assert_eq!(
        timers.next_word(),
        Some(TimerWord::Schedule {
            correlation_id: 3,
            deadline: 30
        })
    );
    timers.clear();
    // To the bound, and not past it.
    for id in 0..i64::try_from(MAX_SERVICE_TIMERS).expect("the bound") {
        assert_eq!(timers.load(id, id), Ok(()));
    }
    assert_eq!(timers.len(), MAX_SERVICE_TIMERS);
    assert_eq!(
        timers.load(i64::MAX, 0),
        Err(TimerOutOfOrder),
        "one past the bound"
    );
    assert_eq!(timers.schedule(i64::MAX, 0), Err(TimersFull));
    assert_eq!(timers.schedule(7, 99), Ok(()), "a pending id at the bound");
}

#[test]
fn a_handling_begun_inside_another_ends_the_first_as_not_taken() {
    let mut timers = Box::new(ServiceTimers::new());
    timers.begin_entry();
    timers.schedule(1, 10).expect("room");
    timers.schedule(2, 20).expect("room");
    assert_eq!(timers.end_entry(true), 0);
    // A handed event whose handler cancels another timer and schedules a third, and
    // is never ended: the next handling finds every timer as it was.
    assert_eq!(timers.begin_event(1, 10), TimerEventIs::Handed);
    timers.cancel(2);
    timers.schedule(3, 30).expect("room");
    assert_eq!(timers.begin_event(1, 10), TimerEventIs::Handed);
    assert_eq!(timers.iter().collect::<Vec<_>>(), [(1, 10), (2, 20)]);
    assert_eq!(timers.end_entry(true), 0);
    assert_eq!(timers.iter().collect::<Vec<_>>(), [(2, 20)]);
    // An end with no handling begun changes nothing.
    assert_eq!(timers.end_entry(true), 0);
    assert_eq!(timers.end_entry(false), 0);
    assert_eq!(timers.iter().collect::<Vec<_>>(), [(2, 20)]);
}

#[test]
fn a_word_the_stream_took_late_takes_off_only_what_it_said() {
    let mut timers = Box::new(ServiceTimers::new());
    timers.begin_entry();
    timers.schedule(1, 10).expect("room");
    timers.schedule(2, 20).expect("room");
    assert_eq!(timers.end_entry(true), 0);
    let first = timers.next_word().expect("timer 1 is to tell");
    assert_eq!(
        first.message(),
        cavalry_proto::cluster::Message::ScheduleTimer {
            correlation_id: 1,
            deadline: 10
        }
    );
    // The timer is scheduled for another deadline before the stream's answer is
    // known: the word that was sent said the old one, so the timer stays to tell.
    timers.begin_entry();
    timers.schedule(1, 11).expect("room");
    assert_eq!(timers.end_entry(true), 0);
    timers.told(first);
    let again = TimerWord::Schedule {
        correlation_id: 1,
        deadline: 11,
    };
    assert_eq!(timers.next_word(), Some(again));
    // A word for a timer that is pending no more, and a cancel that is not the
    // oldest held, change nothing.
    timers.told(TimerWord::Schedule {
        correlation_id: 9,
        deadline: 11,
    });
    timers.told(TimerWord::Cancel { correlation_id: 1 });
    assert_eq!(timers.next_word(), Some(again));
    // Told out of the order of the ids, each word takes its own mark off.
    timers.told(TimerWord::Schedule {
        correlation_id: 2,
        deadline: 20,
    });
    assert_eq!(timers.next_word(), Some(again));
    timers.told(again);
    assert_eq!(timers.next_word(), None);
    // A cancel is the message that says it.
    timers.begin_entry();
    timers.cancel(2);
    assert_eq!(timers.end_entry(true), 0);
    let cancel = timers.next_word().expect("a cancel is held");
    assert_eq!(
        cancel.message(),
        cavalry_proto::cluster::Message::CancelTimer { correlation_id: 2 }
    );
}

#[test]
fn a_refresh_inside_a_handling_is_kept_for_its_end_and_a_clear_keeps_the_hold() {
    let mut timers = Box::new(ServiceTimers::new());
    timers.begin_entry();
    timers.schedule(1, 10).expect("room");
    assert_eq!(timers.end_entry(true), 0);
    let one = timers.next_word().expect("timer 1 is to tell");
    timers.told(one);
    assert_eq!(timers.next_word(), None);
    // A refresh after a handler's first change, with the entry taken and with it
    // not taken: timer 1 is sent again either way.
    for taken in [true, false] {
        timers.begin_entry();
        timers.schedule(2, 20).expect("room");
        timers.tell_all_again();
        assert_eq!(timers.next_word(), None, "nothing inside a handling");
        assert_eq!(timers.end_entry(taken), 0);
        assert_eq!(timers.next_word(), Some(one), "taken: {taken}");
        while let Some(word) = timers.next_word() {
            timers.told(word);
        }
        timers.begin_entry();
        timers.cancel(2);
        assert_eq!(timers.end_entry(true), 0);
        while let Some(word) = timers.next_word() {
            timers.told(word);
        }
    }
    // A clear of a table with a cancel held, a handling open and a refresh waiting
    // leaves nothing of them, and a service that was reading history reads it still.
    timers.begin_entry();
    timers.cancel(1);
    assert_eq!(timers.end_entry(true), 0);
    timers.hold();
    timers.begin_entry();
    timers.schedule(5, 50).expect("room");
    timers.tell_all_again();
    timers.clear();
    assert!(timers.is_empty());
    assert_eq!(
        timers.end_entry(true),
        0,
        "no handling is open after a clear"
    );
    assert_eq!(timers.load(7, 70), Ok(()));
    assert_eq!(timers.next_word(), None, "history is read still");
    timers.release();
    assert_eq!(
        timers.next_word(),
        Some(TimerWord::Schedule {
            correlation_id: 7,
            deadline: 70
        })
    );
}
