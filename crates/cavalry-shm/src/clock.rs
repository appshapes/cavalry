//! The two clocks a process reads (`spec/client-protocol.md`, "Liveness and timeouts";
//! ADR 0109).
//!
//! The **epoch clock** is the host's wall clock. It is for a value another process or a
//! later run reads: the consumer heartbeat, a file's start timestamp, a recording's
//! timestamps. The host's clock can be set, forward or back, so no interval is measured
//! on it.
//!
//! The **nano clock** is for an interval one process measures by itself: every timeout
//! but the driver timeout. It never goes back and a step of the host's clock does not
//! move it. Its readings count from the process's first reading, so they mean nothing to
//! another process, and one stored where an epoch time belongs is decades old, which
//! every check of a heartbeat refuses.
//!
//! Both are an `i64` of nanoseconds; the name of the function is what tells them apart.
//!
//! ```
//! use cavalry_shm::clock;
//!
//! let started = clock::nano_time();
//! let stamp = clock::epoch_time();
//! assert!(clock::nano_time() >= started, "the nano clock never goes back");
//! assert!(stamp > 1_600_000_000_000_000_000, "the epoch clock is the wall clock: {stamp}");
//! ```

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// The epoch clock: the host's wall clock in nanoseconds since the Unix epoch, as every
/// timestamp in `spec/`; 0 before the epoch. A step of the host's clock moves it, so it
/// is read for a value another process compares with its own reading, and never for an
/// interval.
///
/// Inlined into its caller, as the clock read it replaces was: a driver thread reads a
/// clock once per duty cycle, and without link-time optimisation a function of another
/// crate is a call unless it is marked so.
#[inline]
pub fn epoch_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

/// The nano clock: nanoseconds since this process first read it. It never goes back,
/// and a step of the host's clock does not move it, so every interval a process
/// measures by itself is the difference of two readings of it. Every thread of the
/// process reads the one clock, so a reading one thread hands to another compares with
/// the other's own. The first reading is 0.
///
/// It is the standard library's `Instant`: `CLOCK_MONOTONIC` on Linux and
/// `CLOCK_UPTIME_RAW` on macOS, neither of which counts the time a host is suspended.
/// After the first call a reading is one load of the origin and one read of that clock,
/// with no lock and no system call where the operating system serves the clock from
/// user space, as Linux and macOS do. It is inlined into its caller, as
/// [`epoch_time`] is; the origin is one static of this crate wherever it is inlined.
#[inline]
pub fn nano_time() -> i64 {
    let origin = *ORIGIN.get_or_init(Instant::now);
    i64::try_from(origin.elapsed().as_nanos()).unwrap_or(i64::MAX)
}

/// The nano clock's origin: the process's first reading of it.
static ORIGIN: OnceLock<Instant> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::{epoch_time, nano_time};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn the_nano_clock_never_goes_back_and_keeps_time() {
        let first = nano_time();
        let mut last = first;
        for _ in 0..10_000 {
            let now = nano_time();
            assert!(now >= last, "{now} after {last}");
            last = now;
        }
        std::thread::sleep(Duration::from_millis(20));
        let slept = nano_time() - first;
        assert!(slept >= 20_000_000, "20 ms of sleep read as {slept} ns");
    }

    #[test]
    fn every_thread_reads_the_one_nano_clock() {
        let before = nano_time();
        let other = std::thread::spawn(nano_time).join().expect("the thread");
        let after = nano_time();
        assert!(
            before <= other && other <= after,
            "{before} <= {other} <= {after}"
        );
    }

    #[test]
    fn a_nano_reading_is_no_epoch_time() {
        // The origin is the process's first reading, so a nano reading taken for an
        // epoch time is decades old: the mistake fails the first heartbeat check.
        let nano = nano_time();
        let epoch = epoch_time();
        assert!(
            epoch - nano > 1_500_000_000_000_000_000,
            "nano {nano}, epoch {epoch}"
        );
    }

    #[test]
    fn the_epoch_clock_is_the_wall_clock() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_nanos();
        let read = epoch_time();
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_nanos();
        assert!(
            before <= read as u128 && read as u128 <= after,
            "{before} <= {read} <= {after}"
        );
    }
}
