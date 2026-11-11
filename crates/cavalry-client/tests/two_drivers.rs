//! Two drivers started on one directory at nearly the same moment: exactly one runs,
//! the other is refused with `driver-active`, and a client of the one that runs gets
//! a message through (`spec/client-protocol.md`, "The driver directory"; ADR 0079).
//! Invariant 5: the second start must not break the first. Invariant 3: a start under
//! contention degrades to a refusal.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Offer};
use cavalry_driver::{Driver, DriverError};

/// A directory of its own per round, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(round: usize) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-two-drivers-{}-{round}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the round's directory");
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How many rounds the test runs: 100, or `CAVALRY_TWO_DRIVERS_ROUNDS` for the 1,000
/// the decision's evidence names. The rounds spread the second start evenly over the
/// 20 ms after the first, whatever their number.
fn rounds() -> usize {
    std::env::var("CAVALRY_TWO_DRIVERS_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|rounds| *rounds > 0)
        .unwrap_or(100)
}

/// What one round ended with, in words, for the tally a failure prints.
fn outcome(first: &Result<Driver, DriverError>, second: &Result<Driver, DriverError>) -> String {
    let say = |result: &Result<Driver, DriverError>| match result {
        Ok(_) => "started".to_string(),
        Err(error) => format!("refused ({})", error.reason()),
    };
    format!("the first {}, the second {}", say(first), say(second))
}

#[test]
fn of_two_drivers_started_on_one_directory_one_runs_and_the_other_is_refused_as_active() {
    let rounds = rounds();
    let mut wrong: Vec<String> = Vec::new();
    for round in 0..rounds {
        let dir = TestDir::new(round);
        let driver_dir = dir.path().join("driver");
        // The second starts this long after the first: from 0 to just under 20 ms.
        let offset = Duration::from_micros(
            20_000 * u64::try_from(round).expect("a round")
                / u64::try_from(rounds).expect("a count"),
        );
        let barrier = Arc::new(Barrier::new(2));
        let start = |name: &'static str, wait: Duration| {
            let (barrier, driver_dir) = (Arc::clone(&barrier), driver_dir.clone());
            // A log directory each, outside the driver directory, so that the log
            // directory's lock is not what refuses the second.
            let log_dir = dir.path().join(name);
            std::thread::spawn(move || {
                barrier.wait();
                let begin = Instant::now();
                while begin.elapsed() < wait {
                    std::hint::spin_loop();
                }
                Driver::start(&driver_dir, &log_dir)
            })
        };
        let (first, second) = (
            start("log-first", Duration::ZERO),
            start("log-second", offset),
        );
        let (first, second) = (
            first.join().expect("the first start returns"),
            second.join().expect("the second start returns"),
        );
        let said = outcome(&first, &second);
        let (running, refused) = match (first, second) {
            (Ok(driver), Err(error)) | (Err(error), Ok(driver)) => (driver, error),
            (Ok(first), Ok(second)) => {
                wrong.push(format!("round {round}, {offset:?}: {said}"));
                drop(first);
                drop(second);
                continue;
            }
            (Err(_), Err(_)) => {
                wrong.push(format!("round {round}, {offset:?}: {said}"));
                continue;
            }
        };
        if !matches!(refused, DriverError::Active { .. }) {
            wrong.push(format!("round {round}, {offset:?}: {said}: {refused}"));
            drop(running);
            continue;
        }
        // The one that runs serves a client: a message goes through.
        let served = (|| -> Result<(), String> {
            let mut client = Cavalry::connect_to(&driver_dir).map_err(|e| e.to_string())?;
            let mut subscription = client
                .add_subscription("cavalry:ipc", 2500)
                .map_err(|e| e.to_string())?;
            let mut publication = client
                .add_publication("cavalry:ipc", 2500)
                .map_err(|e| e.to_string())?;
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut received = 0;
            let mut sent = false;
            while received == 0 {
                if Instant::now() > deadline {
                    return Err("no message arrived in ten seconds".to_string());
                }
                if !sent {
                    match publication.offer(b"one message") {
                        Ok(_) => sent = true,
                        Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {}
                        Err(other) => return Err(other.to_string()),
                    }
                }
                received += subscription.poll(|_, _| {}, 1).map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        if let Err(why) = served {
            wrong.push(format!(
                "round {round}, {offset:?}: {said}, and the client: {why}"
            ));
        }
        drop(running);
    }
    assert!(
        wrong.is_empty(),
        "{} of {rounds} rounds did not end with one driver running, the other refused as \
         driver-active and a message through:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
