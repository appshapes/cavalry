//! What a thread does on a duty cycle that found no work (CLAUDE.md, "Data plane":
//! busy-spin, yield and backoff, backoff the default): [`IdleStrategy`] names the three,
//! and [`Idler`] is one of them with its state, which each of the driver's threads
//! holds and a program's own duty cycle beside the driver can.

use std::time::Duration;

/// The conductor's idle strategy. In Aeron, these are `BusySpinIdleStrategy`,
/// `YieldingIdleStrategy` and `BackoffIdleStrategy` (`docs/glossary.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum IdleStrategy {
    /// Spin on the CPU: the lowest latency, a whole core. `doctor` recommends it on an
    /// isolated core.
    BusySpin,
    /// Yield the CPU to another thread on every idle cycle.
    Yield,
    /// Spin a few times, then yield a few times, then park for longer and longer, up to
    /// a millisecond, until there is work again. The default.
    #[default]
    Backoff,
}

impl IdleStrategy {
    /// Every strategy, in counter-value order: backoff, yield, busy-spin.
    pub const ALL: [IdleStrategy; 3] = [
        IdleStrategy::Backoff,
        IdleStrategy::Yield,
        IdleStrategy::BusySpin,
    ];

    /// The strategy's name, `backoff`, `yield` or `busy-spin`: the words `CLAUDE.md` and
    /// the glossary use, what a configuration value spells and a `doctor` finding says.
    pub const fn name(self) -> &'static str {
        match self {
            IdleStrategy::BusySpin => "busy-spin",
            IdleStrategy::Yield => "yield",
            IdleStrategy::Backoff => "backoff",
        }
    }

    /// The strategy `name` names, or `None` for a name this version does not know. The
    /// inverse of [`IdleStrategy::name`], exact: `spin` and `Busy-Spin` are refused, and
    /// the caller lists [`IdleStrategy::ALL`] by name in its message.
    pub fn parse(name: &str) -> Option<Self> {
        IdleStrategy::ALL
            .into_iter()
            .find(|strategy| strategy.name() == name)
    }

    /// The value the driver stores in the `conductor.idle_strategy` counter when it
    /// starts, 0 backoff, 1 yield, 2 busy-spin (`counters.toml`), so that `doctor` reads
    /// the strategy a running driver uses from the counters file alone (invariant 4).
    pub const fn counter_value(self) -> i64 {
        match self {
            IdleStrategy::Backoff => 0,
            IdleStrategy::Yield => 1,
            IdleStrategy::BusySpin => 2,
        }
    }

    /// The strategy a `conductor.idle_strategy` value names, or `None` for a value this
    /// version does not know: a newer driver's, or a corrupt file.
    pub const fn from_counter_value(value: i64) -> Option<Self> {
        match value {
            0 => Some(IdleStrategy::Backoff),
            1 => Some(IdleStrategy::Yield),
            2 => Some(IdleStrategy::BusySpin),
            _ => None,
        }
    }
}

/// Aeron's backoff figures: spins, then yields, then parks from a microsecond to a
/// millisecond, doubling.
const MAX_SPINS: u32 = 10;
const MAX_YIELDS: u32 = 20;
const MIN_PARK: Duration = Duration::from_micros(1);
const MAX_PARK: Duration = Duration::from_millis(1);

/// An [`IdleStrategy`] with its state: how far into the backoff an idle streak has gone.
/// A thread that runs a duty cycle holds one, calls [`Idler::idle`] after a cycle that
/// found no work and [`Idler::reset`] after one that did. Each thread of the driver
/// holds one, and a program that runs a duty cycle of its own beside the driver holds
/// one too, so that it idles as the driver does and the backoff's figures are written
/// once: `cavalry run` holds one for the thread of its cluster member. In Aeron, this
/// is an instance of an `IdleStrategy`, with its `idle()` and `reset()`
/// (`docs/glossary.md`, idle strategy).
///
/// The backoff spins 10 times, then yields 20 times, then parks from 1 µs to 1 ms,
/// doubling, until a reset: Aeron's `BackoffIdleStrategy` figures.
///
/// ```
/// use cavalry_driver::{IdleStrategy, Idler};
///
/// // A duty cycle that finds work on its fortieth turn, and says how much it did.
/// let mut turns = 0;
/// let mut duty_cycle = || {
///     turns += 1;
///     usize::from(turns == 40)
/// };
///
/// let mut idler = Idler::new(IdleStrategy::Backoff);
/// let mut idled = 0;
/// loop {
///     if duty_cycle() > 0 {
///         // Work: the next idle streak starts from spinning again.
///         idler.reset();
///         break;
///     }
///     // No work: spin, then yield, then park for longer and longer.
///     idler.idle();
///     idled += 1;
/// }
/// assert_eq!(idled, 39);
/// ```
#[derive(Debug)]
pub struct Idler {
    strategy: IdleStrategy,
    spins: u32,
    yields: u32,
    park: Duration,
}

impl Idler {
    /// An idler of `strategy`, at the start of an idle streak.
    pub const fn new(strategy: IdleStrategy) -> Self {
        Self {
            strategy,
            spins: 0,
            yields: 0,
            park: MIN_PARK,
        }
    }

    /// Called after a duty cycle with no work: busy-spin spins once and yield yields
    /// once, and backoff takes the next step of its streak, which is a park of at most
    /// 1 ms once the spins and the yields are spent.
    ///
    /// The park is [`std::thread::park_timeout`], not a sleep, so that whoever stops the
    /// loop can end it: an [`unpark`](std::thread::Thread::unpark) of this thread makes
    /// the park return at once, and one that came before it makes the next park return
    /// at once. A loop that calls this reads its stop flag every pass, so a stop is the
    /// flag stored and then the unpark, and it does not depend on the park's own
    /// timeout. That timeout is not sure on every machine: on one Mac a wait of 1 ms in
    /// about 20 million did not return until its thread was woken (ADR 0171;
    /// `docs/how-to/find-a-wait-that-does-not-return.md`). A park may also return
    /// early for no reason, which costs one duty cycle.
    pub fn idle(&mut self) {
        match self.strategy {
            IdleStrategy::BusySpin => std::hint::spin_loop(),
            IdleStrategy::Yield => std::thread::yield_now(),
            IdleStrategy::Backoff => {
                if self.spins < MAX_SPINS {
                    self.spins += 1;
                    std::hint::spin_loop();
                } else if self.yields < MAX_YIELDS {
                    self.yields += 1;
                    std::thread::yield_now();
                } else {
                    #[cfg(test)]
                    let park = tests::PARK_FOR_TEST.get().unwrap_or(self.park);
                    #[cfg(not(test))]
                    let park = self.park;
                    std::thread::park_timeout(park);
                    self.park = (self.park * 2).min(MAX_PARK);
                }
            }
        }
    }

    /// Called after a duty cycle that did work: the next idle streak starts from spinning.
    pub fn reset(&mut self) {
        self.spins = 0;
        self.yields = 0;
        self.park = MIN_PARK;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};

    thread_local! {
        /// For a test: how long this thread's backoff parks, in place of at most 1 ms.
        /// An hour stands for the park whose timeout never comes, so that a test passes
        /// only when something unparks the thread.
        pub(crate) static PARK_FOR_TEST: Cell<Option<Duration>> = const { Cell::new(None) };
    }

    /// A park that would last an hour.
    pub(crate) const NEVER: Duration = Duration::from_secs(3600);

    #[test]
    fn a_backoff_that_has_parked_returns_when_its_thread_is_unparked() {
        let stop = Arc::new(AtomicBool::new(false));
        let parking = Arc::new(AtomicBool::new(false));
        let (ended, end) = mpsc::channel();
        let handle = {
            let (stop, parking) = (Arc::clone(&stop), Arc::clone(&parking));
            std::thread::spawn(move || {
                PARK_FOR_TEST.set(Some(NEVER));
                let mut idler = Idler::new(IdleStrategy::Backoff);
                let mut idled = 0u32;
                while !stop.load(Ordering::Acquire) {
                    if idled == MAX_SPINS + MAX_YIELDS {
                        parking.store(true, Ordering::Release);
                    }
                    idler.idle();
                    idled += 1;
                }
                let _ = ended.send(idled);
            })
        };
        while !parking.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        // The stop of every loop that idles here: the flag, then the unpark. Whether the
        // unpark comes before the park or during it, the park returns.
        stop.store(true, Ordering::Release);
        handle.thread().unpark();
        let idled = end
            .recv_timeout(Duration::from_secs(20))
            .expect("the idler's loop ended when its thread was unparked, not an hour later");
        assert_eq!(
            idled,
            MAX_SPINS + MAX_YIELDS + 1,
            "one park, ended by the unpark"
        );
        handle.join().expect("the thread ended");
    }

    #[test]
    fn every_strategy_round_trips_through_its_name_and_its_counter_value() {
        for (index, strategy) in IdleStrategy::ALL.into_iter().enumerate() {
            assert_eq!(IdleStrategy::parse(strategy.name()), Some(strategy));
            assert_eq!(strategy.counter_value(), index as i64);
            assert_eq!(
                IdleStrategy::from_counter_value(index as i64),
                Some(strategy)
            );
        }
        assert_eq!(IdleStrategy::default(), IdleStrategy::Backoff);
        assert_eq!(IdleStrategy::Backoff.counter_value(), 0, "the default is 0");
    }

    #[test]
    fn an_unknown_name_or_value_is_none() {
        for name in [
            "spin",
            "Busy-Spin",
            "BUSY-SPIN",
            "busy_spin",
            "",
            " backoff",
        ] {
            assert_eq!(IdleStrategy::parse(name), None, "{name:?}");
        }
        assert_eq!(IdleStrategy::from_counter_value(3), None);
        assert_eq!(IdleStrategy::from_counter_value(-1), None);
    }
}
