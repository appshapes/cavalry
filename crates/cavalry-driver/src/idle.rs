//! What the conductor does on a duty cycle that found no work (CLAUDE.md, "Data
//! plane": busy-spin, yield and backoff, backoff the default).

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
    /// Spin a few times, then yield a few times, then sleep for longer and longer, up to
    /// a millisecond, until there is work again. The default.
    #[default]
    Backoff,
}

/// Aeron's backoff figures: spins, then yields, then parks from a microsecond to a
/// millisecond, doubling.
const MAX_SPINS: u32 = 10;
const MAX_YIELDS: u32 = 20;
const MIN_PARK: Duration = Duration::from_micros(1);
const MAX_PARK: Duration = Duration::from_millis(1);

/// The strategy with its state: how far into the backoff an idle streak has gone.
#[derive(Debug)]
pub(crate) struct Idler {
    strategy: IdleStrategy,
    spins: u32,
    yields: u32,
    park: Duration,
}

impl Idler {
    pub(crate) const fn new(strategy: IdleStrategy) -> Self {
        Self {
            strategy,
            spins: 0,
            yields: 0,
            park: MIN_PARK,
        }
    }

    /// Called after a duty cycle with no work.
    pub(crate) fn idle(&mut self) {
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
                    std::thread::sleep(self.park);
                    self.park = (self.park * 2).min(MAX_PARK);
                }
            }
        }
    }

    /// Called after a duty cycle that did work: the next idle streak starts from spinning.
    pub(crate) fn reset(&mut self) {
        self.spins = 0;
        self.yields = 0;
        self.park = MIN_PARK;
    }
}
