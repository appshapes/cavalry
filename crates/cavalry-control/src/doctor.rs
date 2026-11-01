//! `cavalry doctor`: explains a driver's state from its directory alone, counters first
//! (CLAUDE.md, invariant 4). [`examine`] opens the counters file and the command ring
//! read-only, as the layouts' pages say a reader does, runs every check, and returns a
//! [`Report`] whose findings each name the docs anchor that explains them;
//! [`examine_with`] adds the log directory: its backing, every recording's two positions,
//! and the catalog slots recovery could not repair (ADR 0025). Nothing here writes, and
//! nothing here is on the data plane.
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

use cavalry_driver::{IdleStrategy, spec};
use cavalry_log::catalog::CatalogReader;
use cavalry_log::layout::{Descriptor, State as SlotState, locate};
use cavalry_proto::udp::{DATAGRAMS_PER_BURST, LINGER, MTU_LENGTH_DEFAULT, SOCKET_RCVBUF_LENGTH};
use cavalry_shm::command_ring::{CommandRing, CommandRingError, layout::DRIVER_CLOSED};
use cavalry_shm::counters::generated::{
    CHANNEL_PUBLICATIONS, CHANNEL_SOCKET_RCVBUF, CHANNEL_SOCKET_SNDBUF, CHANNEL_STATUS,
    CONDUCTOR_IDLE_STRATEGY as IDLE_STRATEGY_COUNTER, COUNTERS, COUNTERS_ALLOCATED,
    COUNTERS_ALLOCATION_FAILED, PUBLICATION_POSITION, RECORDING_BYTES, RECORDING_INVALIDATED,
    RECORDING_POSITION, RECORDING_SYNCED_POSITION, RECORDING_WRITE_FAILED, SENDER_LIMIT,
};
use cavalry_shm::counters::{CounterId, CountersError, CountersReader, Scope};

use crate::config::generated::{
    CONDUCTOR_IDLE_STRATEGY as IDLE_STRATEGY_KEY, DIR as DIR_KEY, LOG_DIR as LOG_DIR_KEY,
};

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

/// Filesystem types that are not a disk of this host's own: an overlay a container
/// loses, or a network mount, where a sync and an advisory lock mean little. A log
/// directory on one is a finding (ADR 0025).
const OVERLAY_OR_NETWORK_FILESYSTEMS: [&str; 8] = [
    "overlay",
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "9p",
    "fuse.sshfs",
    "afs",
];

/// The least free space under the log directory worth no finding: eight segment files
/// of 128 MiB, 1 GiB (ADR 0025).
const LOG_FREE_WARNING: u64 = 8 * 128 * 1024 * 1024;

/// The least share of its filesystem free under the log directory worth no finding
/// naming a purge: one part in this many, a tenth (ADR 0031).
const LOG_FREE_SHARE: u64 = 10;

/// The segment files one `purge segments` command removes within the client's 10 second
/// response timeout, one file's work per duty cycle, in the order of ten thousand (ADR
/// 0031); a finding that names a larger purge says to send it as several.
const SEGMENTS_PER_PURGE: i64 = 10_000;

const DOC_DURABLE_LOG_DRIVER: &str = "docs/reference/durable-log.md#the-driver";
const DOC_RECORDING_COMMANDS: &str = "docs/reference/client-protocol.md#commands";
const DOC_UDP_SENDER: &str = "docs/reference/udp.md#the-sender";
const DOC_UDP_VALUES: &str = "docs/reference/udp.md#values";

/// The least send buffer a send endpoint's socket needs: one datagram of the MTU,
/// Aeron's rule (`UdpChannelTransport` refuses an `SO_SNDBUF` below the MTU), and on
/// Linux a burst, the sixteen datagrams one `sendmmsg` hands the socket in one call
/// (`spec/udp.md`, "Values"; ADR 0041). Elsewhere the sender sends one `sendmsg` per
/// datagram, so a burst never meets the buffer, and macOS's default, 9216 bytes from
/// `net.inet.udp.maxdgram`, holds one datagram: it is not a cap, and a burst as the
/// line there would name every send endpoint on a Mac.
#[cfg(target_os = "linux")]
const SNDBUF_LEAST: i64 = DATAGRAMS_PER_BURST as i64 * MTU_LENGTH_DEFAULT as i64;
#[cfg(not(target_os = "linux"))]
const SNDBUF_LEAST: i64 = MTU_LENGTH_DEFAULT as i64;

/// What [`SNDBUF_LEAST`] is, for the finding.
#[cfg(target_os = "linux")]
const SNDBUF_LEAST_WHAT: &str = "a burst, sixteen datagrams of the MTU in one sendmmsg";
#[cfg(not(target_os = "linux"))]
const SNDBUF_LEAST_WHAT: &str = "one datagram of the MTU";

/// What raises a send buffer below [`SNDBUF_LEAST`]. The driver sets no send buffer
/// (`spec/udp.md`, "Values"; ADR 0041), so the socket holds the operating system's
/// default for a new UDP socket, and the sysctl that moves it is the default's, not the
/// cap on `setsockopt(SO_SNDBUF)`, a call the driver never makes: on Linux
/// `net.core.wmem_default`, since `wmem_max` caps a buffer a socket asks for and does
/// not move one that asked for nothing; on macOS `net.inet.udp.maxdgram`, the name XNU
/// gives its `udp_sendspace` (`net.inet.udp.sendspace` is no oid on macOS 15), the buffer
/// a new UDP socket is given there.
#[cfg(target_os = "linux")]
const SNDBUF_REMEDY: &str = "raise net.core.wmem_default with sysctl (wmem_max caps a \
                             buffer a socket asks for, and the driver asks for none)";
#[cfg(target_os = "macos")]
const SNDBUF_REMEDY: &str = "raise net.inet.udp.maxdgram with sysctl";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const SNDBUF_REMEDY: &str = "raise the operating system's default send buffer for a new UDP socket";

/// The receive buffer the driver asks of a send endpoint's socket (`spec/udp.md`,
/// "Values"), as the socket reads it back when the host granted the ask: on Linux
/// `getsockopt(SO_RCVBUF)` returns twice what `setsockopt` set, the kernel's allowance
/// for its own bookkeeping, so a host that granted the ask reads back twice it, and a
/// readback below that is `net.core.rmem_max` below the ask; elsewhere the socket reads
/// back what was set.
#[cfg(target_os = "linux")]
const RCVBUF_EXPECTED: i64 = 2 * SOCKET_RCVBUF_LENGTH as i64;
#[cfg(not(target_os = "linux"))]
const RCVBUF_EXPECTED: i64 = SOCKET_RCVBUF_LENGTH as i64;

/// What raises a receive buffer below [`RCVBUF_EXPECTED`]: the host's cap on a buffer
/// a socket asks for, which is what the driver does for this one.
#[cfg(target_os = "linux")]
const RCVBUF_REMEDY: &str = "raise net.core.rmem_max with sysctl";
#[cfg(target_os = "macos")]
const RCVBUF_REMEDY: &str = "raise kern.ipc.maxsockbuf with sysctl";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const RCVBUF_REMEDY: &str = "raise the operating system's cap on a socket's receive buffer";

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
/// The log directory's checks need its path: [`examine_with`].
pub fn examine(dir: &Path) -> Report {
    examine_with(dir, None)
}

/// [`examine`], and with `log_dir` the log directory's checks too: the length of its
/// path, its backing, whether
/// the driver's user can write it and how much room it has (`log-directory-backing`,
/// the `log.dir` key's check), under a tenth of its filesystem free with the purge that
/// frees the most (`log-directory-space`), the catalog slots recovery set to invalid,
/// named (`recording-invalid`), every recording's two positions from the counters file,
/// with a recorder a full window behind its publisher (`recording-stalled`) and failed
/// writes (`recording-write-failed`) as findings (ADR 0025), and a recording over half
/// its filesystem, live by its gauge or stopped by its descriptor
/// (`recording-over-half`, ADR 0031).
pub fn examine_with(dir: &Path, log_dir: Option<&Path>) -> Report {
    let mut findings = Vec::new();
    let mut checks = 0;
    if let Some(log_dir) = log_dir {
        checks += log_directory_checks(log_dir, &mut findings);
    }

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

    checks += counters_checks(dir, running, log_dir, &mut findings);
    checks += host_checks(&mut findings);
    finish(dir, findings, checks)
}

/// The `log.dir` key's check: the directory in memory loses every recording at a reboot;
/// on an overlay or a network filesystem a sync and a lock mean little; a directory the
/// driver's user cannot write, or with less than eight segment files of room, stops the
/// next recording. A directory that does not exist yet is a note: the driver creates it.
fn log_directory_checks(log_dir: &Path, findings: &mut Vec<Finding>) -> usize {
    // The path's length first, directory or not: the recorder opens each segment file
    // by its full path, and the standard library allocates a C string for a path of
    // 384 bytes or more instead of using its stack buffer, so a roll on the recorder
    // thread would allocate (`cavalry_log::recorder::LOG_DIR_PATH_LIMIT`).
    let mut checks = 1;
    let length = log_dir.as_os_str().len();
    if length > cavalry_log::recorder::LOG_DIR_PATH_LIMIT {
        findings.push(Finding {
            check: "log-directory-backing",
            level: Level::Warn,
            message: format!(
                "the log directory's path {} is {length} bytes, over the {} under which a segment \
                 roll opens its file without allocating on the recorder thread (the standard \
                 library opens a path on its stack only under 384 bytes, and the longest segment \
                 name is {}); set {}=<a path of at most {} bytes>, or --log-dir",
                log_dir.display(),
                cavalry_log::recorder::LOG_DIR_PATH_LIMIT,
                cavalry_log::recorder::LONGEST_SEGMENT_NAME,
                LOG_DIR_KEY.env,
                cavalry_log::recorder::LOG_DIR_PATH_LIMIT
            ),
            doc: LOG_DIR_KEY.anchor,
        });
    }
    checks += 1;
    if !log_dir.is_dir() {
        findings.push(Finding {
            check: "log-directory-backing",
            level: Level::Note,
            message: format!(
                "no log directory at {} yet; the driver creates it when it starts, under its \
                 lock",
                log_dir.display()
            ),
            doc: LOG_DIR_KEY.anchor,
        });
        return checks;
    }
    let canonical = log_dir
        .canonicalize()
        .unwrap_or_else(|_| log_dir.to_path_buf());
    match backing(&canonical) {
        Backing::Memory => findings.push(Finding {
            check: "log-directory-backing",
            level: Level::Warn,
            message: format!(
                "the log directory {} is in memory: a reboot loses every recording; set {}=<a \
                 directory on a disk with room for the recordings>, or --log-dir",
                log_dir.display(),
                LOG_DIR_KEY.env
            ),
            doc: LOG_DIR_KEY.anchor,
        }),
        Backing::Disk { fstype, mount }
            if OVERLAY_OR_NETWORK_FILESYSTEMS.contains(&fstype.as_str()) =>
        {
            findings.push(Finding {
                check: "log-directory-backing",
                level: Level::Warn,
                message: format!(
                    "the log directory {} is on {fstype} at {mount}, an overlay or network \
                     filesystem, where a sync does not make a byte durable on this host and the \
                     catalog's lock may not hold against another driver; set {}=<a directory on \
                     a disk of this host>, or --log-dir",
                    log_dir.display(),
                    LOG_DIR_KEY.env
                ),
                doc: LOG_DIR_KEY.anchor,
            })
        }
        Backing::Disk { .. } => {}
        Backing::Unknown => findings.push(Finding {
            check: "log-directory-backing",
            level: Level::Note,
            message: format!(
                "whether {} is on a disk of this host cannot be read here: on macOS the default \
                 is under the home directory, on disk; on Linux doctor reads the mount",
                log_dir.display()
            ),
            doc: LOG_DIR_KEY.anchor,
        }),
    }
    // Writable, without writing: the driver creates the catalog and every segment file in
    // the directory, so the directory's mode bits must let this user add entries; and
    // opening the catalog for writing is what the driver's lock needs, when there is one.
    checks += 1;
    let catalog = log_dir.join(cavalry_log::layout::CATALOG_FILE_NAME);
    if let Some(user) = UserIds::current()
        && let Ok(metadata) = std::fs::metadata(log_dir)
        && !user.may_add_entries(&metadata)
    {
        findings.push(Finding {
            check: "log-directory-backing",
            level: Level::Warn,
            message: format!(
                "the log directory {} cannot be written by this user (uid {}): its mode is \
                 {:o}, owner uid {}, group gid {}; the driver cannot create the catalog or a \
                 segment file there; run the driver as a user that owns the log directory, \
                 give the directory to it, or set {}=<a directory it can write>, or --log-dir",
                log_dir.display(),
                user.uid,
                std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o7777,
                std::os::unix::fs::MetadataExt::uid(&metadata),
                std::os::unix::fs::MetadataExt::gid(&metadata),
                LOG_DIR_KEY.env
            ),
            doc: LOG_DIR_KEY.anchor,
        });
    } else if catalog.is_file()
        && let Err(error) = std::fs::OpenOptions::new().append(true).open(&catalog)
    {
        findings.push(Finding {
            check: "log-directory-backing",
            level: Level::Warn,
            message: format!(
                "the catalog at {} cannot be opened for writing by this user: {error}; the \
                 driver must run as a user that owns the log directory, or the directory must \
                 be given to it",
                catalog.display()
            ),
            doc: LOG_DIR_KEY.anchor,
        });
    }
    checks += 1;
    let filesystem = filesystem_bytes(&canonical);
    if let Some(filesystem) = filesystem
        && filesystem.free < LOG_FREE_WARNING
    {
        findings.push(Finding {
            check: "log-directory-backing",
            level: Level::Warn,
            message: format!(
                "the filesystem under {} has {} MiB free, less than eight segment files of \
                 128 MiB: the next recording, or the next segment of a live one, cannot be \
                 created; free space, or set {}=<a directory with room>",
                log_dir.display(),
                filesystem.free / (1024 * 1024),
                LOG_DIR_KEY.env
            ),
            doc: LOG_DIR_KEY.anchor,
        });
    }
    checks += low_space_check(log_dir, filesystem, findings);
    checks
}

/// This process's effective user and groups, read from `id -u` and `id -G`, which every
/// Unix has; `None` when either cannot be run or read, which is not a finding.
struct UserIds {
    uid: u32,
    gids: Vec<u32>,
}

impl UserIds {
    fn current() -> Option<UserIds> {
        let id = |flag: &str| -> Option<Vec<u32>> {
            let output = std::process::Command::new("id").arg(flag).output().ok()?;
            if !output.status.success() {
                return None;
            }
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .map(|field| field.parse().ok())
                .collect()
        };
        let uid = *id("-u")?.first()?;
        Some(UserIds {
            uid,
            gids: id("-G")?,
        })
    }

    /// Whether the mode bits of a directory let this user create an entry in it: write
    /// and search, from the owner's, the group's or the others' bits, the first class
    /// that applies, as the kernel decides; root may always. An ACL is not read.
    fn may_add_entries(&self, metadata: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if self.uid == 0 {
            return true;
        }
        let mode = metadata.permissions().mode();
        let bits = if metadata.uid() == self.uid {
            mode >> 6
        } else if self.gids.contains(&metadata.gid()) {
            mode >> 3
        } else {
            mode
        };
        bits & 0o3 == 0o3
    }
}

/// The bytes of the filesystem under a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Filesystem {
    /// Its size.
    total: u64,
    /// The bytes free to this user.
    free: u64,
}

/// The filesystem under `path`, read from `df -P -k`, which every Unix has; `None`
/// when it cannot be run or read, which is not a finding.
fn filesystem_bytes(path: &Path) -> Option<Filesystem> {
    let output = std::process::Command::new("df")
        .args(["-P", "-k"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // POSIX output: a header, then one line whose second field is the 1K blocks and
    // whose fourth is the available ones.
    let line = text.lines().nth(1)?;
    let mut fields = line.split_whitespace();
    let total: u64 = fields.nth(1)?.parse().ok()?;
    let available: u64 = fields.nth(1)?.parse().ok()?;
    Some(Filesystem {
        total: total.saturating_mul(1024),
        free: available.saturating_mul(1024),
    })
}

/// A stopped recording's segment files, from its descriptor with no file read: the
/// segments from the retained position's to the one holding the stop position's last
/// byte, each of the segment file length (ADR 0031).
#[derive(Clone, Copy, Debug)]
struct StoppedRecording {
    recording_id: i64,
    /// The segment files it holds, by its descriptor.
    segments: i64,
    /// The segment files a `purge segments` to `purge_position` removes: those whose
    /// bytes lie wholly before the stop.
    purgeable: i64,
    segment_file_length: i64,
    /// The highest segment base at or below the stop position: the position a purge
    /// names, since a purge position must be a segment base and the stop is one only
    /// when the recording stopped on a boundary.
    purge_position: i64,
}

impl StoppedRecording {
    /// `None` for a recording that is live, deleted or invalid.
    fn from_descriptor(descriptor: &Descriptor) -> Option<Self> {
        if descriptor.state() != SlotState::Valid || descriptor.is_live() {
            return None;
        }
        let length = i64::from(descriptor.segment_file_length());
        let retained = descriptor.retained_position();
        let stop = descriptor.stop_position();
        let retained_base = locate(descriptor, retained).ok()?.segment_base_position;
        let held = (stop - retained_base).max(0);
        let purgeable = held / length;
        Some(StoppedRecording {
            recording_id: descriptor.recording_id(),
            segments: (held + length - 1) / length,
            purgeable,
            segment_file_length: length,
            purge_position: retained_base.saturating_add(purgeable.saturating_mul(length)),
        })
    }

    fn bytes(&self) -> u64 {
        u64::try_from(self.segments.saturating_mul(self.segment_file_length)).unwrap_or(0)
    }

    fn mib(&self, segments: i64) -> i64 {
        segments.saturating_mul(self.segment_file_length) / (1024 * 1024)
    }
}

/// The stopped recordings of the catalog at `log_dir`; none when there is no catalog
/// or it cannot be read, which is not this check's finding.
fn stopped_recordings(log_dir: &Path) -> Vec<StoppedRecording> {
    let Ok(catalog) = CatalogReader::open(log_dir) else {
        return Vec::new();
    };
    let Ok(slots) = catalog.slots() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for slot in 0..slots {
        let Ok(recording_id) = i64::try_from(slot) else {
            break;
        };
        if let Ok(Some(descriptor)) = catalog.get(recording_id)
            && let Some(recording) = StoppedRecording::from_descriptor(&descriptor)
        {
            found.push(recording);
        }
    }
    found
}

/// The purge that frees a stopped recording's segment files, for a finding.
fn purge_advice(recording: &StoppedRecording) -> String {
    let several = if recording.purgeable > SEGMENTS_PER_PURGE {
        format!(
            ", sent as several purges of at most {SEGMENTS_PER_PURGE} segments each, one \
             command's work within the client's response timeout"
        )
    } else {
        String::new()
    };
    format!(
        "`cavalry record --purge {} --before {}` removes the {} segment files before that \
         position, the highest segment base at or below its stop, {} MiB{several}, and \
         `cavalry record --delete {}` removes every file of it",
        recording.recording_id,
        recording.purge_position,
        recording.purgeable,
        recording.mib(recording.purgeable),
        recording.recording_id
    )
}

/// The free-space check (ADR 0031): under a tenth of the filesystem free, the finding
/// names the stopped recording whose segment files a purge would free the most of and
/// the `purge segments` command that removes them. Apart from
/// [`log_directory_checks`] so a test can give it a filesystem.
fn low_space_check(
    log_dir: &Path,
    filesystem: Option<Filesystem>,
    findings: &mut Vec<Finding>,
) -> usize {
    let Some(filesystem) = filesystem else {
        return 1;
    };
    if filesystem.free.saturating_mul(LOG_FREE_SHARE) >= filesystem.total {
        return 1;
    }
    let largest = stopped_recordings(log_dir)
        .into_iter()
        .filter(|recording| recording.purgeable > 0)
        .max_by_key(|recording| recording.purgeable);
    let advice = match largest {
        Some(recording) => format!(
            "recording {} holds the most segment files a purge frees, {} of {} MiB; {}",
            recording.recording_id,
            recording.purgeable,
            recording.mib(1),
            purge_advice(&recording)
        ),
        None => format!(
            "no stopped recording holds a segment file a purge frees; free space, or set \
             {}=<a directory with room>, or --log-dir",
            LOG_DIR_KEY.env
        ),
    };
    findings.push(Finding {
        check: "log-directory-space",
        level: Level::Warn,
        message: format!(
            "the filesystem under {} has {} MiB free of {} MiB, under a tenth: {advice}",
            log_dir.display(),
            filesystem.free / (1024 * 1024),
            filesystem.total / (1024 * 1024)
        ),
        doc: DOC_RECORDING_COMMANDS,
    });
    1
}

/// The check of each recording's bytes against its filesystem (ADR 0031): one over
/// half of it, from the `recording.bytes` gauge while it is live and from its
/// descriptor once stopped, is the one to purge before the filesystem fills. Apart from
/// [`recording_checks`] so a test can give it a filesystem.
fn over_half_checks(
    log_dir: &Path,
    filesystem: Option<Filesystem>,
    live: &[(i64, i64)],
    findings: &mut Vec<Finding>,
) -> usize {
    let Some(filesystem) = filesystem else {
        return 1;
    };
    let half = filesystem.total / 2;
    let total_mib = filesystem.total / (1024 * 1024);
    for &(recording_id, bytes) in live {
        if u64::try_from(bytes).unwrap_or(0) > half {
            findings.push(Finding {
                check: "recording-over-half",
                level: Level::Warn,
                message: format!(
                    "live recording {recording_id} takes {} MiB, over half of the {total_mib} \
                     MiB filesystem under {}: stop it with `cavalry record --stop \
                     {recording_id}` before the filesystem fills, then `cavalry record --purge \
                     {recording_id} --before <position or instant>` removes its segment files \
                     before that point",
                    bytes / (1024 * 1024),
                    log_dir.display()
                ),
                doc: DOC_RECORDING_COMMANDS,
            });
        }
    }
    for recording in stopped_recordings(log_dir) {
        if recording.bytes() > half {
            findings.push(Finding {
                check: "recording-over-half",
                level: Level::Warn,
                message: format!(
                    "recording {} takes {} MiB by its descriptor, {} segment files, over half \
                     of the {total_mib} MiB filesystem under {}: {}",
                    recording.recording_id,
                    recording.mib(recording.segments),
                    recording.segments,
                    log_dir.display(),
                    purge_advice(&recording)
                ),
                doc: DOC_RECORDING_COMMANDS,
            });
        }
    }
    1
}

/// The counters file: full or filling, then every static counter whose `doctor` text
/// applies when it is above 0, then the recordings.
fn counters_checks(
    dir: &Path,
    running: bool,
    log_dir: Option<&Path>,
    findings: &mut Vec<Finding>,
) -> usize {
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
    let read = |name: &'static str, id: CounterId, findings: &mut Vec<Finding>| {
        read_static(&reader, name, id, findings)
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
    // The counters with a check of their own below are reported there, never a second
    // time here.
    for def in COUNTERS.iter().filter(|def| {
        def.scope == Scope::Static
            && !def.doctor.is_empty()
            && def.id != COUNTERS_ALLOCATED
            && def.id != COUNTERS_ALLOCATION_FAILED
            && def.id != RECORDING_WRITE_FAILED
            && def.id != RECORDING_INVALIDATED
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
    checks += recording_checks(&reader, log_dir, findings);
    checks += udp_checks(&reader, running, findings);
    checks
}

/// One send endpoint as the counters file shows it: the `channel.status` record's key
/// and label, and the endpoint's other counters joined by the key.
struct EndpointCounters {
    id: i64,
    channel: String,
    status: i64,
    sndbuf: Option<i64>,
    rcvbuf: Option<i64>,
    publications: Option<i64>,
}

/// The `cavalry:udp` checks ADR 0039 names, from the counters file alone (ADR 0041):
/// a publication no receiver has answered, whose `sender.limit` is 0 while the driver
/// runs, and a send endpoint's socket buffer the host capped, a send buffer below
/// [`SNDBUF_LEAST`] or a receive buffer below [`RCVBUF_EXPECTED`], each finding naming
/// the sysctl of the platform `doctor` runs on, which is the driver's host. Every send
/// endpoint is listed as a note, as recordings are, with its status, its publications
/// and its buffers.
fn udp_checks(reader: &CountersReader, running: bool, findings: &mut Vec<Finding>) -> usize {
    if !running {
        return 0;
    }
    let capacity = reader.capacity();
    let mut endpoints: Vec<EndpointCounters> = Vec::new();
    let mut buffers: Vec<(i64, i32, i64)> = Vec::new();
    let mut unanswered: Vec<(i64, i32, i32, i64)> = Vec::new();
    let endpoint_id = |key: &[u8]| {
        let mut bytes = [0u8; 8];
        if let Some(head) = key.get(..8) {
            bytes.copy_from_slice(head);
        }
        i64::from_le_bytes(bytes)
    };
    for id in 0..capacity {
        let Some(fields) = reader.record(id).ok().flatten() else {
            continue;
        };
        let Ok(value) = reader.read_acquire(id) else {
            continue;
        };
        let type_id = fields.type_id();
        if type_id == CHANNEL_STATUS.0 {
            let channel = fields
                .label()
                .split_once(' ')
                .map_or_else(String::new, |(_, channel)| channel.to_string());
            endpoints.push(EndpointCounters {
                id: endpoint_id(fields.key()),
                channel,
                status: value,
                sndbuf: None,
                rcvbuf: None,
                publications: None,
            });
        } else if type_id == CHANNEL_SOCKET_SNDBUF.0
            || type_id == CHANNEL_SOCKET_RCVBUF.0
            || type_id == CHANNEL_PUBLICATIONS.0
        {
            buffers.push((endpoint_id(fields.key()), type_id, value));
        } else if type_id == SENDER_LIMIT.0 && value == 0 {
            // The label: the name, the correlation id, the session id, the stream id
            // and the endpoint's registration id.
            let mut words = fields.label().split_whitespace().skip(1);
            let (Some(correlation), Some(session), Some(stream), Some(endpoint)) =
                (words.next(), words.next(), words.next(), words.next())
            else {
                continue;
            };
            let (Ok(correlation_id), Ok(session_id), Ok(stream_id), Ok(endpoint_id)) = (
                correlation.parse::<i64>(),
                session.parse::<i32>(),
                stream.parse::<i32>(),
                endpoint.parse::<i64>(),
            ) else {
                continue;
            };
            unanswered.push((correlation_id, session_id, stream_id, endpoint_id));
        }
    }
    for (id, type_id, value) in buffers {
        if let Some(endpoint) = endpoints.iter_mut().find(|endpoint| endpoint.id == id) {
            if type_id == CHANNEL_SOCKET_SNDBUF.0 {
                endpoint.sndbuf = Some(value);
            } else if type_id == CHANNEL_SOCKET_RCVBUF.0 {
                endpoint.rcvbuf = Some(value);
            } else {
                endpoint.publications = Some(value);
            }
        }
    }
    let channel_of = |id: i64| {
        endpoints
            .iter()
            .find(|endpoint| endpoint.id == id)
            .map_or_else(
                || format!("endpoint {id}"),
                |endpoint| endpoint.channel.clone(),
            )
    };
    for (correlation_id, session_id, stream_id, endpoint_id) in &unanswered {
        findings.push(Finding {
            check: "udp-publication-not-connected",
            level: Level::Warn,
            message: format!(
                "publication {correlation_id} on stream {stream_id} (session {session_id}) \
                 sends on {} and no receiver has answered: its sender limit is 0, its setup \
                 frames go unanswered every 100 ms and the publisher reads not connected; \
                 check that a subscription on stream {stream_id} exists on the endpoint's \
                 host, that its driver receives on that port, and that both drivers run the \
                 same version",
                channel_of(*endpoint_id)
            ),
            doc: DOC_UDP_SENDER,
        });
    }
    // A raised sysctl reaches a socket opened after it, and an endpoint's socket is
    // opened once, by the first `add publication` on its channel, and closed when its
    // last publication's linger ends (`spec/udp.md`, "The receiver"), so the step after
    // the sysctl is to close the endpoint and open a new one.
    let reopen = format!(
        "then remove every publication on the channel, wait the {} s linger for the \
         endpoint to close, and add them again",
        LINGER.as_secs()
    );
    for endpoint in &endpoints {
        if let Some(sndbuf) = endpoint.sndbuf
            && sndbuf < SNDBUF_LEAST
        {
            findings.push(Finding {
                check: "udp-socket-buffers",
                level: Level::Warn,
                message: format!(
                    "send endpoint {}: the socket's send buffer is {sndbuf} bytes, below the \
                     {SNDBUF_LEAST} that {SNDBUF_LEAST_WHAT} needs; the driver sets no send \
                     buffer, so this is the operating system's default for a new UDP socket, \
                     which the host lowered: {SNDBUF_REMEDY}, {reopen}; until then expect \
                     sender.send_failed and sender.short_sends",
                    endpoint.channel
                ),
                doc: DOC_UDP_VALUES,
            });
        }
        if let Some(rcvbuf) = endpoint.rcvbuf
            && rcvbuf < RCVBUF_EXPECTED
        {
            findings.push(Finding {
                check: "udp-socket-buffers",
                level: Level::Warn,
                message: format!(
                    "send endpoint {}: the socket's receive buffer reads back {rcvbuf} bytes, \
                     below the {RCVBUF_EXPECTED} a host that granted the {SOCKET_RCVBUF_LENGTH} \
                     asked reads back, so the host capped the ask: {RCVBUF_REMEDY}, {reopen}; \
                     until then expect status messages and NAKs dropped under load",
                    endpoint.channel
                ),
                doc: DOC_UDP_VALUES,
            });
        }
        findings.push(Finding {
            check: "udp-endpoint",
            level: Level::Note,
            message: format!(
                "send endpoint {}: status {}, {} publication(s), send buffer {} bytes, receive \
                 buffer {} bytes",
                endpoint.channel,
                match endpoint.status {
                    0 => "initializing".to_string(),
                    1 => "active".to_string(),
                    2 => "closing".to_string(),
                    -1 => "errored".to_string(),
                    other => other.to_string(),
                },
                endpoint.publications.unwrap_or(0),
                endpoint.sndbuf.unwrap_or(0),
                endpoint.rcvbuf.unwrap_or(0)
            ),
            doc: DOC_UDP_VALUES,
        });
    }
    2
}

/// A static counter's value, or a finding: a counter that cannot be read is never a
/// silent zero, since the file is the one instrument, so a reader that rejects it reports
/// it (spec/counters-file.md); one the file does not hold is a driver older than doctor.
fn read_static(
    reader: &CountersReader,
    name: &'static str,
    id: CounterId,
    findings: &mut Vec<Finding>,
) -> Option<i64> {
    match reader.read(id.record()) {
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
    }
}

/// One recording as the counters file shows it: the `recording.position` record's key
/// and label, and its value.
struct RecordingCounters {
    recording_id: i64,
    session_id: i32,
    stream_id: i32,
    position: i64,
    synced: Option<i64>,
}

/// The recordings: every `recording.position` counter with its `recording.synced_position`,
/// listed; a recorder a full publication window behind its publisher
/// (`recording-stalled`); writes that failed (`recording-write-failed`); and the catalog
/// slots recovery set to invalid, named from the catalog when the log directory is
/// known (`recording-invalid`) (`spec/client-protocol.md`, "Commands"; ADR 0025).
fn recording_checks(
    reader: &CountersReader,
    log_dir: Option<&Path>,
    findings: &mut Vec<Finding>,
) -> usize {
    let mut checks = 0;
    let capacity = reader.capacity();
    let record = |id: u32| reader.record(id).ok().flatten();
    // The instance counters, by type id: a recording's two, and every publication's
    // position, keyed by correlation id, session id and stream id.
    let mut recordings: Vec<RecordingCounters> = Vec::new();
    let mut synced: Vec<([u8; 8], i64)> = Vec::new();
    let mut publications: Vec<(i32, i32, i64)> = Vec::new();
    let mut live_bytes: Vec<(i64, i64)> = Vec::new();
    for id in 0..capacity {
        let Some(fields) = record(id) else { continue };
        let Ok(value) = reader.read_acquire(id) else {
            continue;
        };
        let key = fields.key();
        if fields.type_id() == RECORDING_POSITION.0 {
            // The label: the name, the recording id, the session id and the stream id.
            let mut words = fields.label().split_whitespace().skip(1);
            let (Some(rid), Some(session), Some(stream)) =
                (words.next(), words.next(), words.next())
            else {
                continue;
            };
            let (Ok(recording_id), Ok(session_id), Ok(stream_id)) = (
                rid.parse::<i64>(),
                session.parse::<i32>(),
                stream.parse::<i32>(),
            ) else {
                continue;
            };
            recordings.push(RecordingCounters {
                recording_id,
                session_id,
                stream_id,
                position: value,
                synced: None,
            });
        } else if fields.type_id() == RECORDING_SYNCED_POSITION.0 {
            let mut rid = [0u8; 8];
            rid.copy_from_slice(&key[..8]);
            synced.push((rid, value));
        } else if fields.type_id() == PUBLICATION_POSITION.0 {
            let session = i32::from_le_bytes([key[8], key[9], key[10], key[11]]);
            let stream = i32::from_le_bytes([key[12], key[13], key[14], key[15]]);
            publications.push((session, stream, value));
        } else if fields.type_id() == RECORDING_BYTES.0 {
            // The gauge's label is the position's: the name, then the recording id.
            if let Some(rid) = fields.label().split_whitespace().nth(1)
                && let Ok(recording_id) = rid.parse::<i64>()
            {
                live_bytes.push((recording_id, value));
            }
        }
    }
    for recording in &mut recordings {
        let rid = recording.recording_id.to_le_bytes();
        recording.synced = synced.iter().find(|(key, _)| *key == rid).map(|(_, v)| *v);
    }
    let window = i64::from(spec::IPC_PUBLICATION_WINDOW);
    for recording in &recordings {
        checks += 1;
        let synced = recording
            .synced
            .map_or("unknown".to_string(), |value| value.to_string());
        findings.push(Finding {
            check: "recording",
            level: Level::Note,
            message: format!(
                "recording {} of stream {} session {}: position {}, synced {}",
                recording.recording_id,
                recording.stream_id,
                recording.session_id,
                recording.position,
                synced
            ),
            doc: DOC_RECORDING_COMMANDS,
        });
        let publisher = publications
            .iter()
            .find(|(session, stream, _)| {
                *session == recording.session_id && *stream == recording.stream_id
            })
            .map(|(_, _, position)| *position);
        if let Some(publisher) = publisher
            && publisher.saturating_sub(recording.position) >= window
        {
            findings.push(Finding {
                check: "recording-stalled",
                level: Level::Warn,
                message: format!(
                    "recording {} is {} bytes behind its publisher, a full publication window: \
                     the recorder's disk is slow or failing, and the publisher is \
                     back-pressured until it catches up; check recording.write_failed and the \
                     log directory's disk, and `recording stopped` if it gives up",
                    recording.recording_id,
                    publisher - recording.position
                ),
                doc: DOC_RECORDING_COMMANDS,
            });
        }
    }
    checks += 1;
    if let Some(failed) = read_static(
        reader,
        "recording.write_failed",
        RECORDING_WRITE_FAILED,
        findings,
    ) && failed > 0
    {
        findings.push(Finding {
            check: "recording-write-failed",
            level: Level::Warn,
            message: format!(
                "{failed} recording writes failed, each stopping its recording at the position \
                 written: the log directory's filesystem is full, read-only or failing; free \
                 space, or move the log directory with {}, then start the recording again; \
                 what was written up to the stop is whole and replayable",
                LOG_DIR_KEY.env
            ),
            doc: DOC_RECORDING_COMMANDS,
        });
    }
    checks += 1;
    if let Some(invalid) = read_static(
        reader,
        "recording.invalidated",
        RECORDING_INVALIDATED,
        findings,
    ) && invalid > 0
    {
        let slots = log_dir
            .and_then(|log_dir| CatalogReader::open(log_dir).ok())
            .and_then(|catalog| {
                let slots = catalog.slots().ok()?;
                // A reader reports an invalid slot as `invalid` and reads nothing else
                // from it (spec/durable-log.md, "Descriptor"); that report names the slot.
                let mut named = Vec::new();
                for slot in 0..slots {
                    match catalog.get(i64::try_from(slot).ok()?) {
                        Err(error) if error.reason() == "invalid" => named.push(slot.to_string()),
                        Ok(Some(descriptor)) if descriptor.state() == SlotState::Invalid => {
                            named.push(slot.to_string());
                        }
                        _ => {}
                    }
                }
                Some(named)
            });
        let which = match slots {
            Some(named) if !named.is_empty() => format!("catalog slots {}", named.join(", ")),
            Some(_) => "no slot of the catalog reads as invalid now".to_string(),
            None => format!(
                "run doctor with {} or --log-dir set to the log directory to see which slots",
                LOG_DIR_KEY.env
            ),
        };
        findings.push(Finding {
            check: "recording-invalid",
            level: Level::Warn,
            message: format!(
                "{invalid} catalog slots were set to invalid when the driver opened the log \
                 directory, recordings their files cannot repair ({which}): check the disk, and \
                 restore the directory from a backup if the recording matters; the slots are \
                 never reused"
            ),
            doc: DOC_DURABLE_LOG_DRIVER,
        });
    }
    if let Some(log_dir) = log_dir {
        checks += over_half_checks(log_dir, filesystem_bytes(log_dir), &live_bytes, findings);
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
    use cavalry_log::catalog::CatalogWriter;
    use cavalry_log::layout::{CatalogHeader, Recording};
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

    /// ADR 0039's two `cavalry:udp` checks, from the counters file alone: a publication
    /// whose `sender.limit` is 0 is one no receiver answered, named with its endpoint's
    /// channel from the label's endpoint id; a send buffer below [`SNDBUF_LEAST`] is a
    /// host that lowered the default the driver leaves alone, and the finding names the
    /// sysctl of that default, not `wmem_max`, which does not move a buffer nobody asked
    /// for; a receive buffer below [`RCVBUF_EXPECTED`] is a host that capped the ask,
    /// and the finding names the cap; each half is exercised alone, with the other at
    /// its line, so that a message or a threshold of one half cannot pass for the
    /// other's; and every send endpoint is listed as a note. The figures below the line
    /// are below it on every platform, so the test reads the same on Linux and macOS,
    /// whose lines differ.
    #[test]
    fn a_udp_publication_no_receiver_answered_and_a_capped_socket_buffer_are_warnings() {
        let dir = TestDir::new("udp");
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let endpoint_id = 9i64;
        let channel = "cavalry:udp?endpoint=10.0.0.2:40123";
        let endpoint_key = endpoint_id.to_le_bytes();
        let status = counters
            .allocate(
                CHANNEL_STATUS,
                &endpoint_key,
                &format!("channel.status {channel}"),
            )
            .expect("room");
        status.set(1);
        let sndbuf = counters
            .allocate(
                CHANNEL_SOCKET_SNDBUF,
                &endpoint_key,
                &format!("channel.socket_sndbuf {channel}"),
            )
            .expect("room");
        // Below one datagram of the MTU, so below the line on every platform.
        sndbuf.set(1024);
        let rcvbuf = counters
            .allocate(
                CHANNEL_SOCKET_RCVBUF,
                &endpoint_key,
                &format!("channel.socket_rcvbuf {channel}"),
            )
            .expect("room");
        rcvbuf.set(RCVBUF_EXPECTED);
        let publications = counters
            .allocate(
                CHANNEL_PUBLICATIONS,
                &endpoint_key,
                &format!("channel.publications {channel}"),
            )
            .expect("room");
        publications.set(1);
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&7i64.to_le_bytes());
        key[8..12].copy_from_slice(&5i32.to_le_bytes());
        key[12..].copy_from_slice(&1001i32.to_le_bytes());
        let limit = counters
            .allocate(SENDER_LIMIT, &key, "sender.limit 7 5 1001 9")
            .expect("room");
        limit.set(0);
        let buffer_findings = |report: &Report| -> Vec<String> {
            report
                .findings
                .iter()
                .filter(|f| f.check == "udp-socket-buffers")
                .inspect(|f| assert_eq!(f.level, Level::Warn))
                .inspect(|f| assert_eq!(f.doc, "docs/reference/udp.md#values"))
                .map(|f| f.message.clone())
                .collect()
        };
        let reopen = format!("wait the {} s linger", LINGER.as_secs());

        // No receiver answered, and the send buffer is below the line: two warnings.
        let report = examine(&dir.0);
        let found = finding(&report, "udp-publication-not-connected");
        assert_eq!(found.level, Level::Warn);
        assert!(found.message.contains("stream 1001"), "{}", found.message);
        assert!(found.message.contains(channel), "{}", found.message);
        assert_eq!(found.doc, "docs/reference/udp.md#the-sender");
        let buffers = buffer_findings(&report);
        assert_eq!(buffers.len(), 1, "{report}");
        let message = &buffers[0];
        assert!(message.contains("send buffer is 1024 bytes"), "{message}");
        assert!(
            message.contains(&format!("below the {SNDBUF_LEAST} ")),
            "{message}"
        );
        assert!(message.contains(SNDBUF_REMEDY), "{message}");
        assert!(message.contains(&reopen), "{message}");
        assert!(
            !message.contains("wmem_max with"),
            "wmem_max moves nothing: {message}"
        );
        let note = finding(&report, "udp-endpoint");
        assert_eq!(note.level, Level::Note);
        assert!(note.message.contains("active"), "{}", note.message);
        assert!(note.message.contains("1 publication"), "{}", note.message);
        assert!(report.ok(), "warnings do not fail: {report}");

        // The send buffer is Linux's default, and the receive buffer reads back one
        // byte below what a host that granted the ask reads back: the receive half
        // alone, naming the cap.
        sndbuf.set(212_992);
        rcvbuf.set(RCVBUF_EXPECTED - 1);
        let report = examine(&dir.0);
        let buffers = buffer_findings(&report);
        assert_eq!(buffers.len(), 1, "{report}");
        let message = &buffers[0];
        assert!(
            message.contains(&format!(
                "receive buffer reads back {} bytes",
                RCVBUF_EXPECTED - 1
            )),
            "{message}"
        );
        assert!(
            message.contains(&format!("below the {RCVBUF_EXPECTED} ")),
            "{message}"
        );
        assert!(
            message.contains(&format!("the {SOCKET_RCVBUF_LENGTH} asked")),
            "{message}"
        );
        assert!(message.contains(RCVBUF_REMEDY), "{message}");
        assert!(message.contains(&reopen), "{message}");
        assert!(!message.contains("send buffer"), "{message}");

        // A receiver answered and the host gave both buffers: the note alone remains.
        limit.store_release(131_072);
        rcvbuf.set(RCVBUF_EXPECTED);
        let report = examine(&dir.0);
        assert!(
            !report.findings.iter().any(|f| {
                f.check == "udp-publication-not-connected" || f.check == "udp-socket-buffers"
            }),
            "{report}"
        );
        assert!(report.findings.iter().any(|f| f.check == "udp-endpoint"));
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
        let counters = CountersFile::create(&dir.0, 128).expect("counters");
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

    /// The `log.dir` key's check: a directory not yet created is a note; one that exists
    /// is judged by its backing, in memory a warning, a disk nothing, unreadable a note.
    #[test]
    fn a_missing_log_directory_is_a_note_and_an_existing_one_is_judged_by_its_backing() {
        let dir = TestDir::new("log-dir");
        let missing = dir.0.join("log-missing");
        let report = examine_with(&dir.0, Some(&missing));
        let found = finding(&report, "log-directory-backing");
        assert_eq!(found.level, Level::Note);
        assert!(
            found.message.contains("no log directory"),
            "{}",
            found.message
        );
        assert_eq!(found.doc, LOG_DIR_KEY.anchor);

        let present = dir.0.join("log");
        std::fs::create_dir_all(&present).expect("the log directory");
        let report = examine_with(&dir.0, Some(&present));
        let about: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.check == "log-directory-backing")
            .collect();
        match backing(&present.canonicalize().expect("canonical")) {
            Backing::Memory => {
                assert!(
                    about
                        .iter()
                        .any(|f| f.level == Level::Warn && f.message.contains("in memory")),
                    "{report}"
                );
            }
            Backing::Disk { .. } => {
                assert!(
                    about.iter().all(|f| !f.message.contains("in memory")),
                    "{report}"
                );
            }
            Backing::Unknown => {
                assert!(
                    about
                        .iter()
                        .any(|f| f.level == Level::Note && f.message.contains("cannot be read")),
                    "{report}"
                );
            }
        }
        if cfg!(target_os = "linux") && Path::new("/dev/shm").is_dir() {
            let shm = PathBuf::from(format!(
                "/dev/shm/cavalry-doctor-{}-log",
                std::process::id()
            ));
            std::fs::create_dir_all(&shm).expect("a directory in memory");
            let report = examine_with(&dir.0, Some(&shm));
            let _ = std::fs::remove_dir_all(&shm);
            let found = finding(&report, "log-directory-backing");
            assert_eq!(found.level, Level::Warn);
            assert!(
                found.message.contains("in memory") && found.message.contains(LOG_DIR_KEY.env),
                "{}",
                found.message
            );
        }
    }

    /// The `log.dir` key's check on the path's length: over the limit a segment roll
    /// would allocate on the recorder thread, so doctor warns before the driver runs,
    /// whether or not the directory exists; at the limit it says nothing.
    #[test]
    fn a_log_directory_path_over_the_limit_warns_and_one_at_it_does_not() {
        use cavalry_log::recorder::LOG_DIR_PATH_LIMIT;
        let dir = TestDir::new("log-dir-path");
        let over_the_limit = |report: &Report| {
            report
                .findings
                .iter()
                .find(|f| {
                    f.check == "log-directory-backing"
                        && f.level == Level::Warn
                        && f.message.contains("bytes, over the")
                })
                .cloned()
        };
        let mut long = dir.0.clone();
        while long.as_os_str().len() <= LOG_DIR_PATH_LIMIT {
            long.push("x".repeat(120));
        }
        let report = examine_with(&dir.0, Some(&long));
        let found = over_the_limit(&report).unwrap_or_else(|| panic!("no warning: {report}"));
        assert!(
            found
                .message
                .contains(&format!("is {} bytes", long.as_os_str().len()))
                && found.message.contains(&LOG_DIR_PATH_LIMIT.to_string())
                && found.message.contains(LOG_DIR_KEY.env),
            "{}",
            found.message
        );
        assert_eq!(found.doc, LOG_DIR_KEY.anchor);

        // A test directory already at the limit leaves no room for a path at it exactly.
        let Some(room) = LOG_DIR_PATH_LIMIT.checked_sub(dir.0.as_os_str().len() + 1) else {
            return;
        };
        let mut at = dir.0.clone();
        at.push("y".repeat(room));
        assert_eq!(at.as_os_str().len(), LOG_DIR_PATH_LIMIT);
        let report = examine_with(&dir.0, Some(&at));
        assert!(over_the_limit(&report).is_none(), "{report}");
    }

    /// A log directory this user cannot add entries to warns before any driver has used
    /// it, and so does a catalog this user cannot open for writing; root may write both,
    /// so the test says nothing as root.
    #[test]
    fn a_log_directory_or_catalog_this_user_cannot_write_warns() {
        use std::os::unix::fs::PermissionsExt;
        let user = UserIds::current().expect("id -u and id -G");
        if user.uid == 0 {
            return;
        }
        let dir = TestDir::new("log-dir-mode");
        let log_dir = dir.0.join("log");
        std::fs::create_dir_all(&log_dir).expect("the log directory");
        let unwritable = |report: &Report| {
            report
                .findings
                .iter()
                .find(|f| {
                    f.check == "log-directory-backing"
                        && f.level == Level::Warn
                        && f.message.contains("by this user")
                })
                .cloned()
        };
        let report = examine_with(&dir.0, Some(&log_dir));
        assert!(unwritable(&report).is_none(), "{report}");

        // No catalog yet, a directory of mode 0500: the driver could not create one.
        let set = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod")
        };
        set(&log_dir, 0o500);
        let report = examine_with(&dir.0, Some(&log_dir));
        set(&log_dir, 0o700);
        let found = unwritable(&report).unwrap_or_else(|| panic!("no warning: {report}"));
        assert!(
            found.message.contains("cannot be written")
                && found.message.contains("mode is 500")
                && found.message.contains(LOG_DIR_KEY.env),
            "{}",
            found.message
        );
        assert_eq!(found.doc, LOG_DIR_KEY.anchor);

        // A catalog of mode 0400 in a directory this user may write.
        let catalog = log_dir.join(cavalry_log::layout::CATALOG_FILE_NAME);
        drop(CatalogWriter::open(&log_dir, now_nanos()).expect("the catalog"));
        set(&catalog, 0o400);
        let report = examine_with(&dir.0, Some(&log_dir));
        set(&catalog, 0o600);
        let found = unwritable(&report).unwrap_or_else(|| panic!("no warning: {report}"));
        assert!(
            found.message.contains("cannot be opened for writing"),
            "{}",
            found.message
        );
    }

    /// Every recording is listed with its two positions, and a recorder a full
    /// publication window behind its publisher is `recording-stalled`.
    #[test]
    fn recordings_are_listed_and_a_recorder_a_window_behind_warns() {
        let dir = TestDir::new("recordings");
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&7i64.to_le_bytes());
        key[8..12].copy_from_slice(&(-5i32).to_le_bytes());
        key[12..].copy_from_slice(&1001i32.to_le_bytes());
        let publisher = counters
            .allocate(PUBLICATION_POSITION, &key, "publication.position 7 -5 1001")
            .expect("the publication's position");
        publisher.store_release(40 * 1024 * 1024);
        let rid = 3i64.to_le_bytes();
        let position = counters
            .allocate(RECORDING_POSITION, &rid, "recording.position 3 -5 1001")
            .expect("the recording's position");
        let synced = counters
            .allocate(
                RECORDING_SYNCED_POSITION,
                &rid,
                "recording.synced_position 3 -5 1001",
            )
            .expect("the recording's synced position");
        position.store_release(2 * 1024 * 1024);
        synced.store_release(1024 * 1024);

        let report = examine(&dir.0);
        let listed = finding(&report, "recording");
        assert_eq!(listed.level, Level::Note);
        assert_eq!(
            listed.message,
            "recording 3 of stream 1001 session -5: position 2097152, synced 1048576"
        );
        let stalled = finding(&report, "recording-stalled");
        assert_eq!(stalled.level, Level::Warn);
        assert!(
            stalled
                .message
                .starts_with("recording 3 is 39845888 bytes behind"),
            "{}",
            stalled.message
        );

        position.store_release(40 * 1024 * 1024);
        let report = examine(&dir.0);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "recording-stalled"),
            "caught up: {report}"
        );
    }

    /// `recording-write-failed` and `recording-invalid`, the latter naming the catalog's
    /// invalid slots when the log directory is given, and saying how to see them when not.
    #[test]
    fn failed_writes_and_invalid_slots_are_findings_that_name_the_slots() {
        use std::os::unix::fs::FileExt;
        let dir = TestDir::new("recording-failures");
        let counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        counters
            .counter(RECORDING_WRITE_FAILED.record())
            .expect("static")
            .increment();
        counters
            .counter(RECORDING_INVALIDATED.record())
            .expect("static")
            .add(2);
        let log_dir = dir.0.join("log");
        std::fs::create_dir_all(&log_dir).expect("the log directory");
        let now = now_nanos();
        {
            let mut catalog = CatalogWriter::open(&log_dir, now).expect("the catalog");
            for stream_id in [1, 2] {
                catalog
                    .add(&Recording {
                        recording_id: 0,
                        start_timestamp: now,
                        start_position: 0,
                        initial_term_id: 1,
                        term_length: 65536,
                        mtu_length: 1408,
                        session_id: 1,
                        stream_id,
                        channel: "cavalry:ipc",
                        source_identity: "cavalry:ipc",
                    })
                    .expect("a slot");
            }
        }
        let catalog = std::fs::OpenOptions::new()
            .write(true)
            .open(log_dir.join(cavalry_log::layout::CATALOG_FILE_NAME))
            .expect("the catalog opens");
        catalog
            .write_all_at(
                &(SlotState::Invalid as i32).to_le_bytes(),
                CatalogHeader::slot_offset(0),
            )
            .expect("state 2 on slot 0");

        let report = examine_with(&dir.0, Some(&log_dir));
        let failed = finding(&report, "recording-write-failed");
        assert_eq!(failed.level, Level::Warn);
        assert!(
            failed.message.starts_with("1 recording writes failed"),
            "{}",
            failed.message
        );
        let invalid = finding(&report, "recording-invalid");
        assert_eq!(invalid.level, Level::Warn);
        assert!(
            invalid.message.contains("(catalog slots 0)"),
            "{}",
            invalid.message
        );
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "recording.write_failed" || f.check == "recording.invalidated"),
            "each counter is reported once: {report}"
        );

        let report = examine(&dir.0);
        assert!(
            finding(&report, "recording-invalid")
                .message
                .contains("--log-dir"),
            "{report}"
        );
    }

    #[test]
    fn a_counter_above_zero_brings_its_doctor_text() {
        let dir = TestDir::new("counter");
        let counters = CountersFile::create(&dir.0, 128).expect("counters");
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
        let _counters = CountersFile::create(&dir.0, 128).expect("counters");
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

        // A corrupt record: a state the spec does not define fails that counter's read,
        // and the finding names the page that says what a reader rejects.
        {
            use std::os::unix::fs::FileExt;
            let header = CountersReader::open(&dir.0).expect("opens").header();
            let offset = header.record_offset(last).expect("a record");
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(dir.0.join("counters"))
                .expect("writable");
            file.write_at(&7i32.to_le_bytes(), offset as u64)
                .expect("written");
        }
        let report = examine(&dir.0);
        let found = finding(&report, name);
        assert_eq!(found.level, Level::Fail, "{}", found.message);
        assert_eq!(found.doc, DOC_COUNTERS_REJECTS);
        assert!(
            found.message.contains("cannot be read") && found.message.contains("restart"),
            "{}",
            found.message
        );

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
        let _counters = CountersFile::create(&dir.0, 128).expect("counters");
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
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let statics = COUNTERS
            .iter()
            .filter(|def| def.scope == Scope::Static)
            .count();
        // Up to 115 of 128 allocated is under nine tenths; 116 is over.
        for _ in statics..115 {
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
        assert!(found.message.contains("116 of 128"), "{}", found.message);
        assert!(report.ok());
    }

    #[test]
    fn failures_come_first_in_the_report() {
        let dir = TestDir::new("order");
        let counters = CountersFile::create(&dir.0, 128).expect("counters");
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
        let _counters = CountersFile::create(&dir.0, 128).expect("counters");
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
        let _counters = CountersFile::create(&dir.0, 128).expect("counters");
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
        let counters = CountersFile::create(&dir.0, 128).expect("counters");
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
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let mut n = 0;
        while counters.allocate(CLIENT_TIMED_OUT, &[], "filler").is_ok() {
            n += 1;
            assert!(n < 128);
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

    const MIB: u64 = 1024 * 1024;
    const SEGMENT: i64 = cavalry_log::layout::SEGMENT_FILE_LENGTH as i64;

    /// A catalog of three recordings: 0 stopped 4096 bytes into its fourth segment, 1
    /// stopped after three with its first purged, and 2 live.
    fn catalog_with_three(dir: &TestDir) -> PathBuf {
        let log_dir = dir.0.join("log");
        std::fs::create_dir_all(&log_dir).expect("the log directory");
        let now = now_nanos();
        let mut catalog = CatalogWriter::open(&log_dir, now).expect("the catalog");
        for recording_id in 0..3 {
            catalog
                .add(&Recording {
                    recording_id,
                    start_timestamp: now,
                    start_position: 0,
                    initial_term_id: 1,
                    term_length: 65536,
                    mtu_length: 1408,
                    session_id: 1,
                    stream_id: 1001,
                    channel: "cavalry:ipc",
                    source_identity: "cavalry:ipc",
                })
                .expect("a slot");
        }
        catalog
            .stop(0, 3 * SEGMENT + 4096, now + 1)
            .expect("stopped");
        catalog.stop(1, 3 * SEGMENT, now + 1).expect("stopped");
        catalog.retain(1, SEGMENT).expect("purged");
        log_dir
    }

    fn messages<'a>(findings: &'a [Finding], check: &str) -> Vec<&'a str> {
        findings
            .iter()
            .filter(|finding| finding.check == check)
            .map(|finding| finding.message.as_str())
            .collect()
    }

    #[test]
    fn low_free_space_names_the_stopped_recording_a_purge_frees_the_most_of() {
        let dir = TestDir::new("low-space");
        let log_dir = catalog_with_three(&dir);
        let mut findings = Vec::new();
        let low = Some(Filesystem {
            total: 100 * 1024 * MIB,
            free: 5 * 1024 * MIB,
        });
        assert_eq!(low_space_check(&log_dir, low, &mut findings), 1);
        let found = messages(&findings, "log-directory-space");
        assert_eq!(found.len(), 1, "{findings:?}");
        assert_eq!(
            found[0],
            format!(
                "the filesystem under {} has 5120 MiB free of 102400 MiB, under a tenth: \
                 recording 0 holds the most segment files a purge frees, 3 of 128 MiB; \
                 `cavalry record --purge 0 --before {}` removes the 3 segment files before \
                 that position, the highest segment base at or below its stop, 384 MiB, and \
                 `cavalry record --delete 0` removes every file of it",
                log_dir.display(),
                3 * SEGMENT
            )
        );
        assert_eq!(findings[0].level, Level::Warn);
        assert_eq!(findings[0].doc, DOC_RECORDING_COMMANDS);

        // A tenth free, or the filesystem unknown, is no finding.
        let mut findings = Vec::new();
        let enough = Some(Filesystem {
            total: 100 * 1024 * MIB,
            free: 10 * 1024 * MIB,
        });
        assert_eq!(low_space_check(&log_dir, enough, &mut findings), 1);
        assert_eq!(low_space_check(&log_dir, None, &mut findings), 1);
        assert!(findings.is_empty(), "{findings:?}");

        // With nothing to purge, the finding says so and names the key.
        let empty = TestDir::new("low-space-empty");
        let mut findings = Vec::new();
        low_space_check(&empty.0, low, &mut findings);
        let found = messages(&findings, "log-directory-space");
        assert_eq!(found.len(), 1, "{findings:?}");
        assert!(
            found[0].contains("no stopped recording holds a segment file a purge frees"),
            "{}",
            found[0]
        );
        assert!(found[0].contains(LOG_DIR_KEY.env), "{}", found[0]);
    }

    #[test]
    fn a_recording_over_half_its_filesystem_is_named_live_or_stopped() {
        let dir = TestDir::new("over-half");
        let log_dir = catalog_with_three(&dir);
        let mut findings = Vec::new();
        let filesystem = Some(Filesystem {
            total: 600 * MIB,
            free: 10 * MIB,
        });
        let live = [(7, 400 * MIB as i64), (8, 100 * MIB as i64)];
        assert_eq!(
            over_half_checks(&log_dir, filesystem, &live, &mut findings),
            1
        );
        let found = messages(&findings, "recording-over-half");
        assert_eq!(found.len(), 2, "{findings:?}");
        assert!(
            found[0].starts_with("live recording 7 takes 400 MiB, over half of the 600 MiB"),
            "{}",
            found[0]
        );
        assert!(
            found[0].contains("`cavalry record --stop 7`"),
            "{}",
            found[0]
        );
        assert!(
            found[1].starts_with(
                "recording 0 takes 512 MiB by its descriptor, 4 segment files, over half of \
                 the 600 MiB"
            ),
            "{}",
            found[1]
        );
        assert!(
            found[1].contains("`cavalry record --purge 0 --before"),
            "{}",
            found[1]
        );
        // Recording 1's two segments left, 256 MiB, are under half; so is the unknown.
        let mut findings = Vec::new();
        over_half_checks(&log_dir, None, &live, &mut findings);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn the_filesystem_is_read_from_df() {
        let dir = TestDir::new("df");
        let filesystem = filesystem_bytes(&dir.0).expect("df runs here");
        assert!(filesystem.total >= filesystem.free, "{filesystem:?}");
        assert!(filesystem.total > 0, "{filesystem:?}");
    }
}
