//! What `cavalry run` records for `cavalry doctor` to read once it has stopped.
//!
//! `cavalry run` is a driver and, with the cluster's two keys set, a cluster member
//! beside it. When it gives the member up it closes the driver too, and what the
//! member counted goes with the member: a member's counters are a client's, freed when
//! the client closes (`spec/counters-file.md`, "`counters.toml`"). A static counter is
//! the driver's own and stays in the directory the driver leaves, so the reason is
//! counted there.

use std::path::Path;

use cavalry_shm::counters::generated::RUN_MEMBER_GIVEN_UP;
use cavalry_shm::counters::{CountersError, CountersWriter};

/// Adds one to `run.member_given_up` in the counters file of the driver in `dir`:
/// `cavalry run` gave up the cluster member it ran beside that driver, after the
/// member's duty cycle had failed for 10 s on end or its thread had panicked, and is
/// about to close the driver. The counter is static, so it is in the directory after
/// the driver has closed, where [`crate::doctor`] reports it with what to do next
/// (invariant 4).
///
/// [`CountersError::Unallocated`] when the file's record of that id is not this
/// counter: a file of a driver older than the counter, which never allocated it, and
/// in which another counter may sit there. Nothing is written then. Any other error
/// is the file's: it cannot be opened, or is not a counters file.
///
/// ```
/// use cavalry_control::run::member_given_up;
/// use cavalry_driver::Driver;
/// use cavalry_shm::counters::CountersReader;
/// use cavalry_shm::counters::generated::RUN_MEMBER_GIVEN_UP;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let dir = std::env::temp_dir().join(format!("cavalry-control-run-{}", std::process::id()));
/// let log_dir = dir.with_extension("log");
/// let driver = Driver::start(&dir, &log_dir)?;
/// member_given_up(&dir)?;
/// driver.close()?;
/// // The count is in the directory the closed driver left.
/// let counters = CountersReader::open(&dir)?;
/// assert_eq!(counters.read(RUN_MEMBER_GIVEN_UP.record())?, 1);
/// # std::fs::remove_dir_all(&dir)?;
/// # std::fs::remove_dir_all(&log_dir)?;
/// # Ok(())
/// # }
/// ```
pub fn member_given_up(dir: &Path) -> Result<(), CountersError> {
    let counters = CountersWriter::open(dir)?;
    let record = RUN_MEMBER_GIVEN_UP.record();
    match counters.record(record)? {
        Some(fields) if fields.type_id() == RUN_MEMBER_GIVEN_UP.0 => {
            counters.counter(record)?.increment();
            Ok(())
        }
        _ => Err(CountersError::Unallocated { id: record }),
    }
}
