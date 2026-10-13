//! `cavalry doctor`: explains a driver's state from its directory alone, counters first
//! (CLAUDE.md, invariant 4). [`examine`] opens the counters file and the command ring
//! read-only, as the layouts' pages say a reader does, runs every check, and returns a
//! [`Report`] whose findings each name the docs anchor that explains them. Nothing here
//! writes, and nothing here is on the data plane.
//!
//! ```
//! use cavalry_control::doctor::{Level, examine};
//!
//! // A directory with no driver: one finding, failed, and it says what to do.
//! let dir = std::env::temp_dir().join(format!("cavalry-doctor-doc-{}", std::process::id()));
//! let report = examine(&dir);
//! assert!(!report.ok());
//! assert_eq!(report.findings[0].check, "driver-directory");
//! assert_eq!(report.findings[0].level, Level::Fail);
//! assert!(report.findings[0].message.contains("cavalry run"));
//! assert!(report.findings[0].doc.starts_with("docs/reference/"));
//! println!("{report}");
//! ```

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_driver::IdleStrategy;
use cavalry_shm::command_ring::{CommandRing, CommandRingError, layout::DRIVER_CLOSED};
use cavalry_shm::counters::generated::{
    CONDUCTOR_IDLE_STRATEGY as IDLE_STRATEGY_COUNTER, COUNTERS, COUNTERS_ALLOCATED,
    COUNTERS_ALLOCATION_FAILED,
};
use cavalry_shm::counters::{CounterId, CountersError, CountersReader, Scope};

use crate::config::generated::{CONDUCTOR_IDLE_STRATEGY as IDLE_STRATEGY_KEY, DIR as DIR_KEY};

/// The driver timeout of `spec/client-protocol.md`: a heartbeat older than this is a
/// stopped driver.
const DRIVER_TIMEOUT: Duration = Duration::from_secs(10);

/// The fill of the command ring that is worth a finding.
const RING_FILL_WARNING: f64 = 0.5;

/// The share of the counters file allocated that is worth a finding.
const COUNTERS_FILL_WARNING: f64 = 0.9;

const DOC_DIRECTORY: &str = "docs/reference/client-protocol.md#the-driver-directory";
const DOC_RING_FULL: &str = "docs/reference/command-ring.md#when-the-ring-is-full";
const DOC_RING_REJECTS: &str = "docs/reference/command-ring.md#what-a-reader-rejects";
const DOC_COUNTERS_FULL: &str = "docs/reference/counters-file.md#when-the-file-is-full";
const DOC_COUNTERS: &str = "docs/reference/counters.md";
const DOC_COUNTERS_REJECTS: &str = "docs/reference/counters-file.md#what-a-reader-rejects";
/// The host findings point at the bench host's page until a page on tuning a host for
/// Cavalry exists: it is the one page that says how each setting is changed.
const DOC_HOST: &str = "docs/how-to/set-up-a-bench-host.md";

/// The Linux filesystem types that live in memory: a log buffer on one of these never
/// waits on write-back. `/dev/shm` is a tmpfs on every common distribution.
const MEMORY_FILESYSTEMS: [&str; 3] = ["tmpfs", "ramfs", "hugetlbfs"];

/// How serious a finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// The driver is not working: an operator acts now.
    Fail,
    /// Something is wrong or will be: an operator looks.
    Warn,
    /// A fact worth knowing, or a recommendation.
    Note,
}

impl Level {
    const fn name(self) -> &'static str {
        match self {
            Level::Fail => "FAIL",
            Level::Warn => "WARN",
            Level::Note => "note",
        }
    }
}

/// One finding: which check, how serious, what it means and what to do, and where the
/// docs explain it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The check's name, stable, for a test or a dashboard.
    pub check: &'static str,
    /// How serious.
    pub level: Level,
    /// What was found and what the operator does next.
    pub message: String,
    /// The docs anchor that explains it.
    pub doc: &'static str,
}

/// Everything `doctor` found on one directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The directory examined.
    pub dir: PathBuf,
    /// The findings, failures first.
    pub findings: Vec<Finding>,
    /// How many checks ran.
    pub checks: usize,
}

impl Report {
    /// Whether no check failed.
    pub fn ok(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|finding| finding.level == Level::Fail)
    }

    /// The process exit code `cavalry doctor` ends with: 0 when no check failed, 1
    /// otherwise.
    pub fn exit_code(&self) -> u8 {
        u8::from(!self.ok())
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "cavalry doctor: {}, checks run: {}",
            self.dir.display(),
            self.checks
        )?;
        for finding in &self.findings {
            writeln!(
                f,
                "  {:<4} {:<22} {}; see {}",
                finding.level.name(),
                finding.check,
                finding.message,
                finding.doc
            )?;
        }
        let fails = self
            .findings
            .iter()
            .filter(|finding| finding.level == Level::Fail)
            .count();
        let warns = self
            .findings
            .iter()
            .filter(|finding| finding.level == Level::Warn)
            .count();
        write!(f, "  {fails} failed, {warns} warnings")
    }
}

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

fn seconds(nanos: i64) -> f64 {
    nanos as f64 / 1e9
}

/// Runs every check on the driver directory `dir` and returns the report. A missing
/// directory or a dead driver stops the checks that need a running driver, and says so.
pub fn examine(dir: &Path) -> Report {
    let mut findings = Vec::new();
    let mut checks = 0;

    checks += 1;
    if !dir.is_dir() {
        findings.push(Finding {
            check: "driver-directory",
            level: Level::Fail,
            message: format!(
                "no driver directory at {}; start a driver with `cavalry run`, or point doctor \
                 at the directory the driver uses",
                dir.display()
            ),
            doc: DOC_DIRECTORY,
        });
        let host = host_checks(&mut findings);
        return finish(dir, findings, checks + host);
    }
    checks += backing_check(dir, &mut findings);

    checks += 1;
    let running = match CommandRing::open(dir, false) {
        Ok(ring) => {
            let heartbeat = ring.consumer_heartbeat();
            let age = now_nanos().saturating_sub(heartbeat);
            let pid = ring.header().creator_pid();
            if heartbeat == DRIVER_CLOSED {
                findings.push(Finding {
                    check: "driver-heartbeat",
                    level: Level::Fail,
                    message: format!(
                        "the driver that made {} (pid {pid}) closed; its counters below are the \
                         last it wrote; start a driver with `cavalry run`",
                        dir.display()
                    ),
                    doc: DOC_DIRECTORY,
                });
                false
            } else if age > i64::try_from(DRIVER_TIMEOUT.as_nanos()).unwrap_or(i64::MAX) {
                findings.push(Finding {
                    check: "driver-heartbeat",
                    level: Level::Fail,
                    message: format!(
                        "the driver at {} (pid {pid}) stopped without closing: its heartbeat is \
                         {:.1} s old; the process died or is stalled; check whether pid {pid} is \
                         alive, then start a driver with `cavalry run`",
                        dir.display(),
                        seconds(age)
                    ),
                    doc: DOC_DIRECTORY,
                });
                false
            } else {
                let up = now_nanos().saturating_sub(ring.header().start_timestamp());
                findings.push(Finding {
                    check: "driver-heartbeat",
                    level: Level::Note,
                    message: format!(
                        "the driver at {} (pid {pid}) is running, up {:.0} s, heartbeat {:.2} s old",
                        dir.display(),
                        seconds(up),
                        seconds(age)
                    ),
                    doc: DOC_DIRECTORY,
                });
                checks += 1;
                let used = ring.size();
                let fill = f64::from(used) / f64::from(ring.capacity());
                if fill >= RING_FILL_WARNING {
                    findings.push(Finding {
                        check: "command-ring-fill",
                        level: Level::Warn,
                        message: format!(
                            "the command ring holds {used} of {} bytes ({:.0}%): clients issue \
                             commands faster than the conductor reads them, or the conductor is \
                             stalled; look at the heartbeat above and at the clients",
                            ring.capacity(),
                            fill * 100.0
                        ),
                        doc: DOC_RING_FULL,
                    });
                }
                true
            }
        }
        Err(CommandRingError::NotReady) => {
            findings.push(Finding {
                check: "driver-heartbeat",
                level: Level::Warn,
                message: format!(
                    "the command ring at {} is not ready: a driver is creating it now, or died \
                     while creating it; run doctor again in a second",
                    dir.display()
                ),
                doc: DOC_DIRECTORY,
            });
            false
        }
        Err(error) => {
            findings.push(Finding {
                check: "driver-heartbeat",
                level: Level::Fail,
                message: format!(
                    "the command ring at {} cannot be read: {error}; the directory is not a \
                     driver's, or a driver died while creating it; `cavalry run` remakes it",
                    dir.display()
                ),
                doc: DOC_RING_REJECTS,
            });
            false
        }
    };

    checks += counters_checks(dir, running, &mut findings);
    checks += host_checks(&mut findings);
    finish(dir, findings, checks)
}

/// The counters file: full or filling, then every static counter whose `doctor` text
/// applies when it is above 0.
fn counters_checks(dir: &Path, running: bool, findings: &mut Vec<Finding>) -> usize {
    let reader = match CountersReader::open(dir) {
        Ok(reader) => reader,
        Err(CountersError::NotReady) => {
            findings.push(Finding {
                check: "counters-file",
                level: Level::Warn,
                message: format!(
                    "the counters file at {} is not ready: a driver is creating it now, or died \
                     while creating it; run doctor again in a second",
                    dir.display()
                ),
                doc: DOC_COUNTERS_REJECTS,
            });
            return 1;
        }
        Err(error) => {
            findings.push(Finding {
                check: "counters-file",
                level: if running { Level::Fail } else { Level::Warn },
                message: format!(
                    "the counters file at {} cannot be read: {error}; the file is corrupt, from \
                     another version, or not a driver's; restart the driver, which remakes it",
                    dir.display()
                ),
                doc: DOC_COUNTERS_REJECTS,
            });
            return 1;
        }
    };
    let mut checks = 1;
    let capacity = i64::from(reader.capacity());
    // A counter that cannot be read is a finding, never a silent zero: the file is the
    // one instrument, so a reader that rejects it reports it (spec/counters-file.md).
    let read = |name: &'static str, id: CounterId, findings: &mut Vec<Finding>| match reader
        .read(id.record())
    {
        Ok(value) => Some(value),
        Err(CountersError::Unallocated { .. } | CountersError::Id { .. }) => {
            findings.push(Finding {
                check: name,
                level: Level::Note,
                message: format!(
                    "counter {name} is not in this driver's file: the driver is older than \
                         doctor; upgrade it when convenient"
                ),
                doc: DOC_COUNTERS,
            });
            None
        }
        Err(error) => {
            findings.push(Finding {
                check: name,
                level: Level::Fail,
                message: format!(
                    "counter {name} cannot be read: {error}; the counters file is corrupt or \
                         not a driver's; restart the driver, which remakes it"
                ),
                doc: DOC_COUNTERS_REJECTS,
            });
            None
        }
    };
    let allocated = read("counters.allocated", COUNTERS_ALLOCATED, findings);
    let failed = read(
        "counters.allocation_failed",
        COUNTERS_ALLOCATION_FAILED,
        findings,
    );
    if let (Some(allocated), Some(failed)) = (allocated, failed) {
        if failed > 0 {
            findings.push(Finding {
                check: "counters-file",
                level: Level::Fail,
                message: format!(
                    "{failed} counter allocations were refused: the counters file is full at \
                     {allocated} of {capacity}; publications and subscriptions were refused \
                     with them; close what is not used; the capacity is fixed in this version"
                ),
                doc: DOC_COUNTERS_FULL,
            });
        } else if allocated as f64 >= capacity as f64 * COUNTERS_FILL_WARNING {
            findings.push(Finding {
                check: "counters-file",
                level: Level::Warn,
                message: format!(
                    "the counters file is {allocated} of {capacity} allocated: the next \
                     publications or subscriptions will be refused when it is full; close what \
                     is not used; the capacity is fixed in this version"
                ),
                doc: DOC_COUNTERS_FULL,
            });
        }
    }
    // The two counters read above are reported there, never a second time here.
    for def in COUNTERS.iter().filter(|def| {
        def.scope == Scope::Static
            && !def.doctor.is_empty()
            && def.id != COUNTERS_ALLOCATED
            && def.id != COUNTERS_ALLOCATION_FAILED
    }) {
        checks += 1;
        let Some(value) = read(def.name, def.id, findings) else {
            continue;
        };
        if value > 0 {
            findings.push(Finding {
                check: def.name,
                level: Level::Warn,
                message: format!("{} is {value}: {}", def.name, def.doctor),
                doc: DOC_COUNTERS,
            });
        }
    }
    // The conductor's idle strategy, from the counter the driver stored it in: the check
    // of the `conductor.idle.strategy` key (config.toml). What a stopped driver ran with
    // is nothing to act on, so it is read only while the driver runs.
    if running {
        checks += 1;
        if let Some(value) = read("conductor.idle_strategy", IDLE_STRATEGY_COUNTER, findings) {
            idle_strategy_check(value, findings);
        }
    }
    checks
}

/// The check of the `conductor.idle.strategy` key: busy-spin on a host with no isolated
/// CPU takes a core from the clients for a thread with little to do; backoff on a host
/// with isolated CPUs leaves the limit a publisher waits on to a sleep of up to 1 ms.
fn idle_strategy_check(value: i64, findings: &mut Vec<Finding>) {
    let isolated = isolated_cpus();
    match IdleStrategy::from_counter_value(value) {
        None => findings.push(Finding {
            check: "conductor-idle-strategy",
            level: Level::Warn,
            message: format!(
                "conductor.idle_strategy is {value}, which this doctor does not know: the driver \
                 is newer than doctor, or the counters file is corrupt; upgrade doctor to the \
                 driver's version"
            ),
            doc: IDLE_STRATEGY_KEY.anchor,
        }),
        Some(IdleStrategy::BusySpin) if isolated.is_none() => findings.push(Finding {
            check: "conductor-idle-strategy",
            level: Level::Warn,
            message: format!(
                "the conductor busy-spins and no CPU is isolated: it takes a whole core from the \
                 clients and the rest of the host for a thread with little to do; set {}=backoff, \
                 or isolate a core, pin the driver there and keep busy-spin",
                IDLE_STRATEGY_KEY.env
            ),
            doc: IDLE_STRATEGY_KEY.anchor,
        }),
        Some(strategy @ (IdleStrategy::Backoff | IdleStrategy::Yield)) => {
            if let Some(cpus) = isolated {
                findings.push(Finding {
                    check: "conductor-idle-strategy",
                    level: Level::Note,
                    message: format!(
                        "CPUs {cpus} are isolated and the conductor runs {}: pin the driver there \
                         and set {}=busy-spin, so that the limit a publisher waits on moves without \
                         a sleep of up to 1 ms",
                        strategy.name(),
                        IDLE_STRATEGY_KEY.env
                    ),
                    doc: IDLE_STRATEGY_KEY.anchor,
                });
            }
        }
        Some(IdleStrategy::BusySpin) => {}
    }
}

/// The isolated CPUs of a Linux host, as `/sys/devices/system/cpu/isolated` lists them,
/// or `None` when there are none or the host is not Linux.
fn isolated_cpus() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::fs::read_to_string("/sys/devices/system/cpu/isolated")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// What a driver directory is on.
#[derive(Debug, PartialEq, Eq)]
enum Backing {
    /// A filesystem in memory: one of [`MEMORY_FILESYSTEMS`].
    Memory,
    /// A filesystem that writes back to a device, with its type and mount point.
    Disk { fstype: String, mount: String },
    /// The host does not say: not Linux, or `/proc/self/mountinfo` cannot be read.
    Unknown,
}

/// The check of the `dir` key: a log buffer on a disk-backed filesystem goes through the
/// page cache, and write-back can stall the publisher writing it.
fn backing_check(dir: &Path, findings: &mut Vec<Finding>) -> usize {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    match backing(&canonical) {
        Backing::Memory => {}
        Backing::Disk { fstype, mount } => findings.push(Finding {
            check: "driver-directory-backing",
            level: Level::Warn,
            message: format!(
                "the driver directory {} is on {fstype} at {mount}, not a filesystem in memory: \
                 a publisher's writes to a log buffer go through the page cache, and write-back \
                 can stall it; set {}=<a directory on a tmpfs, /dev/shm on most Linux hosts> for \
                 the driver and its clients, or mount a tmpfs there",
                dir.display(),
                DIR_KEY.env
            ),
            doc: DIR_KEY.anchor,
        }),
        Backing::Unknown => findings.push(Finding {
            check: "driver-directory-backing",
            level: Level::Note,
            message: format!(
                "whether {} is in memory cannot be read on this host: on macOS the directory \
                 is on disk, the best the default can do there; on Linux the default is a tmpfs, \
                 /dev/shm",
                dir.display()
            ),
            doc: DIR_KEY.anchor,
        }),
    }
    1
}

/// What `path` is on, from `/proc/self/mountinfo` on Linux.
fn backing(path: &Path) -> Backing {
    if !cfg!(target_os = "linux") {
        return Backing::Unknown;
    }
    match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(mountinfo) => backing_from_mountinfo(path, &mountinfo),
        Err(_) => Backing::Unknown,
    }
}

/// The mount of the longest mount point that is `path` or a parent of it, as
/// `proc(5)` lays `mountinfo` out: mount point fifth, then the options, optional fields
/// to a `-`, then the filesystem type. A mount point's space, tab, newline and backslash
/// are octal escapes.
fn backing_from_mountinfo(path: &Path, mountinfo: &str) -> Backing {
    let path = path.to_string_lossy();
    let mut best: Option<(String, String)> = None;
    for line in mountinfo.lines() {
        let mut fields = line.split(' ');
        let Some(mount) = fields.nth(4).map(unescape_mount) else {
            continue;
        };
        let Some(fstype) = fields.skip_while(|field| *field != "-").nth(1) else {
            continue;
        };
        let under = mount == "/"
            || *path == *mount
            || path
                .strip_prefix(mount.as_str())
                .is_some_and(|rest| rest.starts_with('/'));
        if under && best.as_ref().is_none_or(|(m, _)| mount.len() > m.len()) {
            best = Some((mount, fstype.to_string()));
        }
    }
    match best {
        Some((_, fstype)) if MEMORY_FILESYSTEMS.contains(&fstype.as_str()) => Backing::Memory,
        Some((mount, fstype)) => Backing::Disk { fstype, mount },
        None => Backing::Unknown,
    }
}

/// `\040` and the other octal escapes of a mount point, decoded.
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(code) = u8::from_str_radix(&field[i + 1..i + 4], 8)
        {
            out.push(code);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The host, on Linux: transparent huge pages, the governor, isolated CPUs and
/// `/dev/shm`. Elsewhere there is nothing to read, and the check says so.
fn host_checks(findings: &mut Vec<Finding>) -> usize {
    if !cfg!(target_os = "linux") {
        return 0;
    }
    let mut checks = 0;
    let read = |path: &str| {
        std::fs::read_to_string(path)
            .ok()
            .map(|text| text.trim().to_string())
    };

    checks += 1;
    if let Some(thp) = read("/sys/kernel/mm/transparent_hugepage/enabled") {
        let setting = thp
            .split_whitespace()
            .find(|word| word.starts_with('['))
            .map_or(thp.clone(), |word| {
                word.trim_matches(['[', ']']).to_string()
            });
        // `madvise` is harmless: the driver never asks for huge pages, so only `always`
        // puts the kernel's compaction under its mappings.
        if setting == "always" {
            findings.push(Finding {
                check: "host-huge-pages",
                level: Level::Warn,
                message: format!(
                    "transparent huge pages are `{setting}`: the kernel may compact memory under \
                     the driver's mappings and stall a publisher; set them to madvise or never"
                ),
                doc: DOC_HOST,
            });
        }
    }

    checks += 1;
    if let Some(governor) = read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        && governor != "performance"
    {
        findings.push(Finding {
            check: "host-governor",
            level: Level::Warn,
            message: format!(
                "the CPU governor is `{governor}`: cores clock down between messages and the \
                 tail grows; set it to performance"
            ),
            doc: DOC_HOST,
        });
    }

    checks += 1;
    if let Some(isolated) = isolated_cpus() {
        findings.push(Finding {
            check: "host-isolated-cpus",
            level: Level::Note,
            message: format!(
                "CPUs {isolated} are isolated: run the driver's threads there with the busy-spin \
                 idle strategy for the lowest tail"
            ),
            doc: DOC_HOST,
        });
    }

    checks += 1;
    if !Path::new("/dev/shm").is_dir() {
        findings.push(Finding {
            check: "host-shm",
            level: Level::Warn,
            message: "/dev/shm is missing: the driver's files go to the temporary directory, \
                      where write-back can stall a publisher; mount a tmpfs at /dev/shm"
                .to_string(),
            doc: DOC_DIRECTORY,
        });
    }
    checks
}

fn finish(dir: &Path, mut findings: Vec<Finding>, checks: usize) -> Report {
    let rank = |level: Level| match level {
        Level::Fail => 0,
        Level::Warn => 1,
        Level::Note => 2,
    };
    findings.sort_by_key(|finding| rank(finding.level));
    Report {
        dir: dir.to_path_buf(),
        findings,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cavalry_shm::command_ring::CommandRing;
    use cavalry_shm::counters::CountersFile;
    use cavalry_shm::counters::generated::CLIENT_TIMED_OUT;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(test: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("cavalry-doctor-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("the test directory");
            TestDir(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn finding<'a>(report: &'a Report, check: &str) -> &'a Finding {
        report
            .findings
            .iter()
            .find(|finding| finding.check == check)
            .unwrap_or_else(|| panic!("no finding {check} in {report}"))
    }

    #[test]
    fn no_directory_is_one_failure_that_says_cavalry_run() {
        let dir = std::env::temp_dir().join(format!("cavalry-doctor-none-{}", std::process::id()));
        let report = examine(&dir);
        assert!(!report.ok());
        assert_eq!(report.exit_code(), 1);
        let found = finding(&report, "driver-directory");
        assert_eq!(found.level, Level::Fail);
        assert!(found.message.contains("cavalry run"));
        assert_eq!(found.doc, DOC_DIRECTORY);
    }

    #[test]
    fn a_closed_and_a_stopped_driver_are_told_apart() {
        let dir = TestDir::new("stopped");
        let counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");

        let report = examine(&dir.0);
        assert!(report.ok(), "{report}");
        assert_eq!(finding(&report, "driver-heartbeat").level, Level::Note);

        consumer.store_heartbeat(now_nanos() - 11_000_000_000);
        let report = examine(&dir.0);
        let found = finding(&report, "driver-heartbeat");
        assert_eq!(found.level, Level::Fail);
        assert!(
            found.message.contains("stopped without closing"),
            "{}",
            found.message
        );

        consumer.close();
        let report = examine(&dir.0);
        let found = finding(&report, "driver-heartbeat");
        assert_eq!(found.level, Level::Fail);
        assert!(found.message.contains("closed"), "{}", found.message);
        drop(counters);
    }

    #[test]
    fn a_counter_above_zero_brings_its_doctor_text() {
        let dir = TestDir::new("counter");
        let counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        counters
            .counter(CLIENT_TIMED_OUT.record())
            .expect("static")
            .increment();
        let report = examine(&dir.0);
        let found = finding(&report, "client.timed_out");
        assert_eq!(found.level, Level::Warn);
        assert!(
            found
                .message
                .starts_with("client.timed_out is 1: A client stopped"),
            "{}",
            found.message
        );
        assert!(report.ok(), "a warning does not fail: {report}");
        assert!(
            report.to_string().contains("WARN client.timed_out"),
            "{report}"
        );
    }

    #[test]
    fn a_counter_the_driver_does_not_have_is_a_note_and_a_corrupt_file_fails() {
        let dir = TestDir::new("older");
        // A driver older than doctor: its file has no record for the last static counter
        // that doctor reports, the last with a doctor text.
        let last = COUNTERS
            .iter()
            .filter(|def| def.scope == Scope::Static && !def.doctor.is_empty())
            .map(|def| def.id.record())
            .max()
            .expect("a static counter");
        let _counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        {
            use std::os::unix::fs::FileExt;
            let header = CountersReader::open(&dir.0).expect("opens").header();
            let offset = header.record_offset(last).expect("a record");
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(dir.0.join("counters"))
                .expect("writable");
            // State 0: unused, as an older driver's file would have it.
            file.write_at(&0i32.to_le_bytes(), offset as u64)
                .expect("written");
        }
        let report = examine(&dir.0);
        let name = COUNTERS
            .iter()
            .find(|def| def.id.record() == last)
            .expect("the def")
            .name;
        let found = finding(&report, name);
        assert_eq!(found.level, Level::Note, "{}", found.message);
        assert!(
            found.message.contains("older than doctor"),
            "{}",
            found.message
        );
        assert!(report.ok(), "{report}");

        // A corrupt file: the magic is wrong, and every read fails loudly.
        {
            use std::os::unix::fs::FileExt;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(dir.0.join("counters"))
                .expect("writable");
            file.write_at(b"NOTCVLY!", 0).expect("written");
        }
        let report = examine(&dir.0);
        let found = finding(&report, "counters-file");
        assert_eq!(found.level, Level::Fail);
        assert_eq!(found.doc, DOC_COUNTERS_REJECTS);
        assert!(
            found.message.contains("cannot be read") && found.message.contains("restart"),
            "{}",
            found.message
        );
    }

    #[test]
    fn the_command_ring_fill_warns_at_half() {
        let dir = TestDir::new("fill");
        let _counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let opened = CommandRing::open(&dir.0, true).expect("opens");
        let mut producer = opened.producer(Duration::from_secs(10)).expect("producer");
        // Records of 512 bytes, 520 aligned: three are below half of 4096, four above.
        for _ in 0..3 {
            producer
                .write(1, &[0u8; 512], now_nanos())
                .expect("written");
        }
        let report = examine(&dir.0);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "command-ring-fill"),
            "{report}"
        );
        producer
            .write(1, &[0u8; 512], now_nanos())
            .expect("written");
        let report = examine(&dir.0);
        let found = finding(&report, "command-ring-fill");
        assert_eq!(found.level, Level::Warn);
        assert!(found.message.contains("2080 of 4096"), "{}", found.message);
    }

    #[test]
    fn the_counters_file_warns_at_nine_tenths() {
        let dir = TestDir::new("nine-tenths");
        let mut counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let statics = COUNTERS
            .iter()
            .filter(|def| def.scope == Scope::Static)
            .count();
        // Up to 57 of 64 allocated is under nine tenths; 58 is over.
        for _ in statics..57 {
            counters
                .allocate(CLIENT_TIMED_OUT, &[], "filler")
                .expect("room");
        }
        let report = examine(&dir.0);
        assert!(
            !report.findings.iter().any(|f| f.check == "counters-file"),
            "{report}"
        );
        counters
            .allocate(CLIENT_TIMED_OUT, &[], "filler")
            .expect("room");
        let report = examine(&dir.0);
        let found = finding(&report, "counters-file");
        assert_eq!(found.level, Level::Warn);
        assert!(found.message.contains("58 of 64"), "{}", found.message);
        assert!(report.ok());
    }

    #[test]
    fn failures_come_first_in_the_report() {
        let dir = TestDir::new("order");
        let counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        counters
            .counter(CLIENT_TIMED_OUT.record())
            .expect("static")
            .increment();
        consumer.store_heartbeat(now_nanos() - 11_000_000_000);
        let report = examine(&dir.0);
        let levels: Vec<Level> = report.findings.iter().map(|f| f.level).collect();
        assert!(
            levels.contains(&Level::Fail) && levels.contains(&Level::Warn),
            "{report}"
        );
        let rank = |level: &Level| match level {
            Level::Fail => 0,
            Level::Warn => 1,
            Level::Note => 2,
        };
        assert!(
            levels
                .windows(2)
                .all(|pair| rank(&pair[0]) <= rank(&pair[1])),
            "{report}"
        );
    }

    #[test]
    fn a_ring_not_yet_ready_warns_and_a_wrong_one_fails() {
        let dir = TestDir::new("ring-state");
        let _counters = CountersFile::create(&dir.0, 64).expect("counters");
        // A zero-filled ring of a plausible length: a creator at work.
        std::fs::write(dir.0.join("command-ring"), vec![0u8; 4096 + 768]).expect("written");
        let report = examine(&dir.0);
        let found = finding(&report, "driver-heartbeat");
        assert_eq!(found.level, Level::Warn);
        assert!(found.message.contains("not ready"), "{}", found.message);
        // A ring too short to be one: not a driver's.
        std::fs::write(dir.0.join("command-ring"), vec![0u8; 100]).expect("written");
        let report = examine(&dir.0);
        let found = finding(&report, "driver-heartbeat");
        assert_eq!(found.level, Level::Fail);
        assert_eq!(found.doc, DOC_RING_REJECTS);
        assert!(
            found.message.contains("cannot be read"),
            "{}",
            found.message
        );
    }

    #[test]
    fn a_counters_file_not_yet_ready_warns() {
        let dir = TestDir::new("counters-not-ready");
        let _counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        // A zero magic in a file of the right length: a creator at work.
        {
            use std::os::unix::fs::FileExt;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(dir.0.join("counters"))
                .expect("writable");
            file.write_at(&[0u8; 8], 0).expect("written");
        }
        let report = examine(&dir.0);
        let found = finding(&report, "counters-file");
        assert_eq!(found.level, Level::Warn);
        assert_eq!(found.doc, DOC_COUNTERS_REJECTS);
        assert!(
            found.message.contains("not ready") && found.message.contains("run doctor again"),
            "{}",
            found.message
        );
        assert!(report.ok(), "a file being created does not fail: {report}");
    }

    const MOUNTINFO: &str = "\
22 28 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:5 - proc proc rw
28 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw,errors=remount-ro
30 22 0:25 / /dev/shm rw,nosuid,nodev shared:4 - tmpfs tmpfs rw,inode64
31 28 0:30 / /mnt/with\\040space rw,relatime shared:9 - tmpfs tmpfs rw,size=1024k
32 28 0:31 / /mnt/huge rw,relatime shared:10 - hugetlbfs nodev rw,pagesize=2M
33 28 8:1 / /data rw,relatime shared:11 - xfs /dev/sda1 rw,attr2
";

    #[test]
    fn the_filesystem_under_a_directory_is_read_from_mountinfo() {
        let under = |path: &str| backing_from_mountinfo(Path::new(path), MOUNTINFO);
        assert_eq!(under("/dev/shm/cavalry-rjae"), Backing::Memory);
        assert_eq!(under("/dev/shm"), Backing::Memory);
        assert_eq!(under("/mnt/with space/cavalry"), Backing::Memory);
        assert_eq!(under("/mnt/huge/d"), Backing::Memory);
        assert_eq!(
            under("/tmp/cavalry-rjae"),
            Backing::Disk {
                fstype: "ext4".to_string(),
                mount: "/".to_string()
            }
        );
        // A mount point is a path component, not a prefix of one.
        assert_eq!(
            under("/dev/shmfoo/cavalry"),
            Backing::Disk {
                fstype: "ext4".to_string(),
                mount: "/".to_string()
            }
        );
        assert_eq!(
            under("/data/cavalry"),
            Backing::Disk {
                fstype: "xfs".to_string(),
                mount: "/data".to_string()
            }
        );
        assert_eq!(
            backing_from_mountinfo(Path::new("/x"), ""),
            Backing::Unknown
        );
        assert_eq!(
            backing_from_mountinfo(Path::new("/x"), "garbage\n"),
            Backing::Unknown
        );
        assert_eq!(unescape_mount("/a\\040b\\011c\\134d"), "/a b\tc\\d");
        assert_eq!(unescape_mount("/a\\04"), "/a\\04");
    }

    #[test]
    fn a_directory_on_disk_warns_and_names_the_variable() {
        let dir = TestDir::new("backing");
        let report = examine(&dir.0);
        let found = finding(&report, "driver-directory-backing");
        assert_eq!(found.doc, "docs/reference/config.md#dir");
        let canonical = dir.0.canonicalize().expect("exists");
        match backing(&canonical) {
            Backing::Memory => panic!("a directory in memory has no finding: {report}"),
            Backing::Disk { fstype, mount } => {
                assert_eq!(found.level, Level::Warn);
                assert!(found.message.contains("CAVALRY_DIR="), "{}", found.message);
                assert!(
                    found.message.contains(&format!("on {fstype} at {mount}")),
                    "{}",
                    found.message
                );
            }
            Backing::Unknown => {
                assert_eq!(found.level, Level::Note);
                assert!(
                    found.message.contains("cannot be read"),
                    "{}",
                    found.message
                );
            }
        }
        // The check runs only on a directory that exists, after driver-directory.
        let missing =
            std::env::temp_dir().join(format!("cavalry-doctor-nodir-{}", std::process::id()));
        let report = examine(&missing);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "driver-directory-backing"),
            "{report}"
        );
    }

    #[test]
    fn a_directory_in_memory_has_no_backing_finding() {
        // Linux hosts with /dev/shm: the default directory's filesystem is in memory.
        if !Path::new("/dev/shm").is_dir() || backing(Path::new("/dev/shm")) != Backing::Memory {
            return;
        }
        let path = PathBuf::from(format!(
            "/dev/shm/cavalry-doctor-{}-memory",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test directory");
        let dir = TestDir(path);
        let report = examine(&dir.0);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "driver-directory-backing"),
            "{report}"
        );
    }

    #[test]
    fn the_idle_strategy_is_judged_against_the_isolated_cpus() {
        let dir = TestDir::new("idle");
        let counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let strategy = counters
            .counter(IDLE_STRATEGY_COUNTER.record())
            .expect("static");
        let idle = |report: &Report| {
            report
                .findings
                .iter()
                .find(|f| f.check == "conductor-idle-strategy")
                .cloned()
        };
        let isolated = isolated_cpus();

        strategy.set(IdleStrategy::BusySpin.counter_value());
        let report = examine(&dir.0);
        match (&isolated, idle(&report)) {
            (None, Some(found)) => {
                assert_eq!(found.level, Level::Warn);
                assert!(
                    found
                        .message
                        .contains("CAVALRY_CONDUCTOR_IDLE_STRATEGY=backoff"),
                    "{}",
                    found.message
                );
                assert_eq!(found.doc, "docs/reference/config.md#conductoridlestrategy");
                assert!(report.ok(), "a warning does not fail: {report}");
            }
            (Some(_), None) => {}
            (isolated, found) => panic!("isolated {isolated:?}, finding {found:?}: {report}"),
        }

        strategy.set(IdleStrategy::Backoff.counter_value());
        let report = examine(&dir.0);
        match (&isolated, idle(&report)) {
            (None, None) => {}
            (Some(cpus), Some(found)) => {
                assert_eq!(found.level, Level::Note);
                assert!(found.message.contains(cpus), "{}", found.message);
                assert!(
                    found
                        .message
                        .contains("CAVALRY_CONDUCTOR_IDLE_STRATEGY=busy-spin"),
                    "{}",
                    found.message
                );
            }
            (isolated, found) => panic!("isolated {isolated:?}, finding {found:?}: {report}"),
        }

        strategy.set(7);
        let report = examine(&dir.0);
        let found = idle(&report).expect("a value doctor does not know is a finding");
        assert_eq!(found.level, Level::Warn);
        assert!(found.message.contains("does not know"), "{}", found.message);

        // A stopped driver's strategy is nothing to act on: the check does not run.
        consumer.close();
        let report = examine(&dir.0);
        assert!(idle(&report).is_none(), "{report}");
    }

    #[test]
    fn a_full_counters_file_fails() {
        let dir = TestDir::new("full");
        let mut counters = CountersFile::create(&dir.0, 64).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let mut n = 0;
        while counters.allocate(CLIENT_TIMED_OUT, &[], "filler").is_ok() {
            n += 1;
            assert!(n < 64);
        }
        let report = examine(&dir.0);
        let found = finding(&report, "counters-file");
        assert_eq!(found.level, Level::Fail);
        assert!(
            found.message.contains("counter allocations were refused"),
            "{}",
            found.message
        );
    }
}
