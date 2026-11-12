//! The driver's reading of the epoch clock beside the nano clock
//! (`spec/client-protocol.md`, "Liveness and timeouts"; ADR 0109).
//!
//! Every driver thread reads the nano clock once per duty cycle
//! ([`crate::conductor::now_nanos`]) and measures every interval between two such
//! readings. A thread that stamps something another process or a later run reads, the
//! conductor its heartbeat and the recorder a recording's timestamps, takes the epoch
//! clock from an [`EpochClock`]: the duty cycle's nano reading plus the difference
//! between the two clocks. The recorder, a data-plane thread, reads that difference
//! again once a millisecond of the nano clock has passed, so it reads one clock on most
//! duty cycles, as it did before the two were told apart, and a step of the host's clock
//! shows in its stamps within a millisecond. The conductor, which no message passes
//! through, reads it on every duty cycle, so that after a step it stores its heartbeat
//! on its next duty cycle and not a millisecond later: a host that wakes from a
//! suspension has clients reading the stepped clock at once.

use cavalry_shm::clock;

/// How long the recorder uses a difference between the two clocks before it reads it
/// again: one millisecond of the nano clock, the cadence of a recording's time index
/// (`spec/durable-log.md`, "Time index").
const READ_INTERVAL: i64 = 1_000_000;

/// Where an [`EpochClock`] takes the difference between the two clocks from.
enum Source {
    /// The host: the epoch clock less the nano clock, read again when the interval of
    /// the nano clock has passed since it was last read.
    Host { interval: i64 },
    /// A difference a test sets, for a duty cycle that is handed a `now` of the test's
    /// own: the epoch clock is then that `now` plus the steps the test has made.
    #[cfg(test)]
    Set,
}

/// The epoch clock as one driver thread reads it.
pub(crate) struct EpochClock {
    source: Source,
    /// The epoch clock less the nano clock, as last read or as a test set it.
    difference: i64,
    /// The nano reading at which the difference was last read from the host.
    read_at: Option<i64>,
}

impl EpochClock {
    /// The host's epoch clock, read once per millisecond: the recorder's. In this
    /// crate's unit tests, which hand a duty cycle a `now` of their own making, it is
    /// that `now` until a test steps it ([`Self::step`]), so that what a test stamps is
    /// what it passed.
    pub(crate) fn new() -> Self {
        Self::reading_every(READ_INTERVAL)
    }

    /// The host's epoch clock, read on every duty cycle: the conductor's. Calls that
    /// share one `now`, as the calls of one duty cycle do, share one reading. In this
    /// crate's unit tests it is the `now` handed in, as [`Self::new`] is.
    pub(crate) fn every_duty_cycle() -> Self {
        Self::reading_every(1)
    }

    fn reading_every(interval: i64) -> Self {
        #[cfg(not(test))]
        let source = Source::Host { interval };
        #[cfg(test)]
        let source = {
            let _ = interval;
            Source::Set
        };
        Self {
            source,
            difference: 0,
            read_at: None,
        }
    }

    /// The host's epoch clock, in a unit test as elsewhere.
    #[cfg(test)]
    fn host(interval: i64) -> Self {
        Self {
            source: Source::Host { interval },
            difference: 0,
            read_at: None,
        }
    }

    /// The epoch clock, nanoseconds since the Unix epoch, on the duty cycle whose nano
    /// reading is `now`.
    pub(crate) fn at(&mut self, now: i64) -> i64 {
        #[cfg(test)]
        let Source::Host { interval } = self.source else {
            return now.saturating_add(self.difference);
        };
        #[cfg(not(test))]
        let Source::Host { interval } = self.source;
        if self
            .read_at
            .is_none_or(|read_at| !(0..interval).contains(&now.saturating_sub(read_at)))
        {
            // One read of the host's clock: `now` is the nano clock's reading on this
            // duty cycle, taken a moment before, and whatever it is, `now` plus this
            // difference is the epoch clock as just read.
            self.difference = clock::epoch_time().saturating_sub(now);
            self.read_at = Some(now);
        }
        now.saturating_add(self.difference)
    }

    /// Steps the epoch clock a test's thread reads by `by` nanoseconds, forward or
    /// back, as a time service or a suspended host steps the host's.
    #[cfg(test)]
    pub(crate) fn step(&mut self, by: i64) {
        self.difference = self.difference.saturating_add(by);
    }
}

#[cfg(test)]
mod tests {
    use super::{EpochClock, READ_INTERVAL};
    use cavalry_shm::clock;

    #[test]
    fn the_hosts_epoch_clock_is_read_once_a_millisecond_and_follows_the_nano_clock_between() {
        let mut epoch = EpochClock::host(READ_INTERVAL);
        let now = clock::nano_time();
        let first = epoch.at(now);
        let host = clock::epoch_time();
        assert!(
            (host - first).abs() < 1_000_000_000,
            "the first reading is the host's: {first} against {host}"
        );
        // Within the interval nothing is read: the stamp moves with `now` alone.
        assert_eq!(epoch.at(now + READ_INTERVAL - 1), first + READ_INTERVAL - 1);
        assert_eq!(epoch.read_at, Some(now));
        // At the interval the difference is read again.
        epoch.at(now + READ_INTERVAL);
        assert_eq!(epoch.read_at, Some(now + READ_INTERVAL));
        // A `now` below the last reading's, which no thread hands it, reads again too.
        epoch.at(now);
        assert_eq!(epoch.read_at, Some(now));
    }

    #[test]
    fn the_conductors_epoch_clock_is_read_on_every_duty_cycle_and_once_in_it() {
        let mut epoch = EpochClock::host(1);
        let now = clock::nano_time();
        let first = epoch.at(now);
        assert_eq!(epoch.read_at, Some(now));
        // The calls of one duty cycle share its `now`, and its reading.
        assert_eq!(epoch.at(now), first);
        // The next duty cycle, a nanosecond on, reads the host again.
        epoch.at(now + 1);
        assert_eq!(epoch.read_at, Some(now + 1));
    }

    #[test]
    fn a_tests_epoch_clock_is_the_now_it_is_handed_plus_its_steps() {
        let mut epoch = EpochClock::new();
        assert_eq!(epoch.at(5_000), 5_000);
        epoch.step(3_600_000_000_000);
        assert_eq!(epoch.at(6_000), 3_600_000_006_000);
        epoch.step(-7_200_000_000_000);
        assert_eq!(epoch.at(7_000), 7_000 - 3_600_000_000_000);
    }
}
