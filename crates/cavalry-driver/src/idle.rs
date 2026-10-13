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

#[cfg(test)]
mod tests {
    use super::*;

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
