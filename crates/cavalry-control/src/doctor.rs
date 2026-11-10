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
use std::net::{Ipv4Addr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_driver::{IdleStrategy, spec};
use cavalry_log::catalog::CatalogReader;
use cavalry_log::layout::{Descriptor, State as SlotState, locate};
use cavalry_proto::cluster::{
    EMPTY_MEMBER, MAX_MEMBERS, STARTUP_CANVASS_TIMEOUT_NS, appointed_leader, parse_members, quorum,
};
#[cfg(target_os = "linux")]
use cavalry_proto::udp::DATAGRAMS_PER_BURST;
use cavalry_proto::udp::{LINGER, MTU_LENGTH_DEFAULT, SOCKET_RCVBUF_LENGTH};
use cavalry_shm::command_ring::{CommandRing, CommandRingError, layout::DRIVER_CLOSED};
use cavalry_shm::counters::generated::{
    CHANNEL_DESTINATIONS, CHANNEL_IMAGES, CHANNEL_PUBLICATIONS, CHANNEL_SOCKET_RCVBUF,
    CHANNEL_SOCKET_SNDBUF, CHANNEL_STATUS, CONDUCTOR_IDLE_STRATEGY as IDLE_STRATEGY_COUNTER,
    COUNTERS, COUNTERS_ALLOCATED, COUNTERS_ALLOCATION_FAILED, IMAGE_CREATED, IMAGE_ENDED,
    IMAGE_HWM_POSITION, IMAGE_REBUILD_POSITION, PUBLICATION_POSITION, RECORDING_ARMED,
    RECORDING_ARMED_DROPPED, RECORDING_ARMED_FAILED, RECORDING_ARMED_STARTED, RECORDING_BYTES,
    RECORDING_INVALIDATED, RECORDING_POSITION, RECORDING_SYNCED_POSITION, RECORDING_WRITE_FAILED,
    SENDER_FLOW_CONTROL_RECEIVERS, SENDER_FLOW_CONTROL_RECEIVERS_FULL,
    SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT, SENDER_LIMIT,
};
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_APPOINTED_LEADER_BEHIND, CLUSTER_COMMIT_POSITION,
    CLUSTER_ELECTION_STATE, CLUSTER_ELECTIONS_FAILED, CLUSTER_JOIN_POSITION_MISMATCHES,
    CLUSTER_LEADER_HEARTBEAT_TIMEOUTS, CLUSTER_LEADER_QUORUM_LOSSES, CLUSTER_LEADERSHIP_TERM_ID,
    CLUSTER_LOG_IMAGES_LOST, CLUSTER_MESSAGES_REJECTED, CLUSTER_ROLE,
};
use cavalry_shm::counters::{CounterId, CountersError, CountersReader, Scope};

use crate::config::Config;
use crate::config::generated::{
    CLUSTER_MEMBER_ID as MEMBER_ID_KEY, CLUSTER_MEMBERS as MEMBERS_KEY,
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
const DOC_UDP_RECEIVER: &str = "docs/reference/udp.md#the-receiver";
const DOC_UDP_VALUES: &str = "docs/reference/udp.md#values";

/// The least MTU an interface needs to carry a datagram of the stream's MTU whole: the
/// datagram and its IPv4 and UDP headers, 20 and 8 bytes (`spec/udp.md`, "Values"; ADR
/// 0044). A datagram longer than the interface's MTU is fragmented by the kernel, and a
/// lost fragment loses the whole datagram, or is dropped where fragmentation is off.
const INTERFACE_MTU_LEAST: u32 = MTU_LENGTH_DEFAULT + 20 + 8;

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
/// not move one that asked for nothing; elsewhere `net.inet.udp.maxdgram`, the name XNU
/// and FreeBSD both give `udp_sendspace` (`net.inet.udp.sendspace` is no oid on macOS
/// 15), the buffer a new UDP socket is given there. Two arms, linux and not-linux, as
/// `cavalry-driver`'s `socket.rs` is written, so that exactly one is live on every
/// target.
#[cfg(target_os = "linux")]
const SNDBUF_REMEDY: &str = "raise net.core.wmem_default with sysctl (wmem_max caps a \
                             buffer a socket asks for, and the driver asks for none)";
#[cfg(not(target_os = "linux"))]
const SNDBUF_REMEDY: &str = "raise net.inet.udp.maxdgram with sysctl";

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
/// a socket asks for, which is what the driver does for this one. On Linux
/// `net.core.rmem_max`; elsewhere `kern.ipc.maxsockbuf`, the cap `sbreserve` applies on
/// macOS and the BSDs alike. Two arms, linux and not-linux, so that exactly one is live
/// on every target.
#[cfg(target_os = "linux")]
const RCVBUF_REMEDY: &str = "raise net.core.rmem_max with sysctl";
#[cfg(not(target_os = "linux"))]
const RCVBUF_REMEDY: &str = "raise kern.ipc.maxsockbuf with sysctl";

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
    examine_in(dir, log_dir, None)
}

/// [`examine_with`], and with `cluster` what the members string says of the cluster the
/// driver's member is in, so that a member with no leader is told whether it is the one
/// that leads ([`examine_config`]).
fn examine_in(dir: &Path, log_dir: Option<&Path>, cluster: Option<&Cluster>) -> Report {
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

    checks += counters_checks(dir, running, log_dir, cluster, &mut findings);
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
    cluster: Option<&Cluster>,
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
            && def.id != RECORDING_ARMED
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
    // An armed recording that started, failed or was dropped is armed no more: the
    // counter's own sentence compares the four, so the check does, and a recording a
    // cluster's follower armed and its leader's image started is nothing to report.
    checks += 1;
    let armed = [
        read("recording.armed", RECORDING_ARMED, findings),
        read("recording.armed_started", RECORDING_ARMED_STARTED, findings),
        read("recording.armed_failed", RECORDING_ARMED_FAILED, findings),
        read("recording.armed_dropped", RECORDING_ARMED_DROPPED, findings),
    ];
    if let [Some(armed), Some(started), Some(failed), Some(dropped)] = armed {
        let waiting = armed - started - failed - dropped;
        if waiting > 0 {
            findings.push(Finding {
                check: "recording.armed",
                level: Level::Warn,
                message: format!(
                    "{waiting} armed recording(s) wait for an image: recording.armed is {armed} \
                     and {started} started, {failed} failed and {dropped} were dropped; check \
                     the publisher and the channel, and that the subscription's endpoint is \
                     the one the publication sends to"
                ),
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
    checks += cluster_checks(&reader, running, cluster, findings);
    checks
}

const DOC_CLUSTER: &str = "docs/how-to/run-a-cluster.md#when-it-does-not-work";

/// A member's counts that no check below reads with the others and whose `counters.toml`
/// doctor sentence says what to do when they are above 0: each is reported by its name
/// with that sentence, as a static counter is (`counters_checks`), since a client
/// counter is not in that loop. A peer of another version shows in the first, a leader
/// gone silent in the second, the leader's log stream lost in the third.
const MEMBER_COUNTS: [CounterId; 3] = [
    CLUSTER_MESSAGES_REJECTED,
    CLUSTER_LEADER_HEARTBEAT_TIMEOUTS,
    CLUSTER_LOG_IMAGES_LOST,
];

/// One cluster member as its counters show it (`spec/cluster.md`, "Counters"): what its
/// consensus module stored, by the member id in each counter's key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MemberCounters {
    id: i32,
    role: Option<i64>,
    election_state: Option<i64>,
    term: Option<i64>,
    append_position: Option<i64>,
    commit_position: Option<i64>,
    elections_failed: i64,
    join_position_mismatches: i64,
    leader_quorum_losses: i64,
    appointed_leader_behind: i64,
    /// The values of [`MEMBER_COUNTS`], in its order.
    counts: [i64; MEMBER_COUNTS.len()],
}

/// What the members string doctor was given says of the cluster: its members' ids, so
/// that doctor knows whether the driver's member is in it, and the appointed leader
/// among them (`spec/cluster.md`, "The appointed leader").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cluster {
    ids: [i32; MAX_MEMBERS],
    count: usize,
}

impl Cluster {
    /// The cluster of a members string, or none when there is no string or the cluster
    /// protocol refuses it, which `cluster-members` reports.
    fn of(members: Option<&str>) -> Option<Cluster> {
        let mut parsed = [EMPTY_MEMBER; MAX_MEMBERS];
        let count = parse_members(members?.as_bytes(), &mut parsed, None).ok()?;
        let mut ids = [0; MAX_MEMBERS];
        for (id, member) in ids.iter_mut().zip(parsed.iter().take(count)) {
            *id = member.id;
        }
        Some(Cluster { ids, count })
    }

    fn ids(&self) -> &[i32] {
        self.ids.get(..self.count).unwrap_or(&[])
    }

    /// The member with the lowest id, the one that leads in this version.
    fn appointed_leader(&self) -> i32 {
        appointed_leader(self.ids())
    }
}

/// The election states of `spec/cluster.md`, "Roles and the election", by their code:
/// every state of the table, those this version's members never enter (2 to 4, 9 and
/// 11 to 13, "The appointed leader") among them, so that a driver newer than this
/// doctor, or a member with the election, is named.
const ELECTION_STATES: [&str; 18] = [
    "init",
    "canvass",
    "nominate",
    "candidate ballot",
    "follower ballot",
    "leader log replication",
    "leader replay",
    "leader init",
    "leader ready",
    "follower log replication",
    "follower replay",
    "follower catch-up init",
    "follower catch-up await",
    "follower catch-up",
    "follower log init",
    "follower log await",
    "follower ready",
    "closed",
];

/// The name of an election state's code, as `spec/cluster.md` names it, or the code
/// itself for one the table does not hold.
fn election_state_name(code: i64) -> String {
    match usize::try_from(code)
        .ok()
        .and_then(|index| ELECTION_STATES.get(index))
    {
        Some(name) => (*name).to_string(),
        None => format!("state {code}, one this doctor does not know"),
    }
}

/// The cluster member this driver is, when it is one, from the counters its consensus
/// module added (`spec/cluster.md`, "Counters"; ADR 0062): who it is and what it knows,
/// a note (`cluster-member`); a member that follows no leader (`cluster-no-leader`); a
/// member its leader's log left behind, which this version cannot bring back
/// (`cluster-joined-late`); an appointed leader that does not lead, or leads and does
/// not close its election, its log behind a member's (`cluster-leader-behind`); a
/// leader that lost its quorum (`cluster-quorum-lost`); and a commit position behind
/// the leader's own append position (`cluster-commit-waits`); and each of
/// [`MEMBER_COUNTS`] above 0, by its counter's name with its doctor sentence, consensus
/// messages rejected among them. A stopped driver's member is gone with its client, so the checks run while the driver runs, and only where a
/// member is. With `cluster`, from the members string doctor was given, a member with no
/// leader is told whether it is the appointed leader.
fn cluster_checks(
    reader: &CountersReader,
    running: bool,
    cluster: Option<&Cluster>,
    findings: &mut Vec<Finding>,
) -> usize {
    if !running {
        return 0;
    }
    // The cluster's counters, by their names in `counters.toml`: their ids are not one
    // range, and each feature of the cluster adds its own after whatever came before.
    let cluster_ids: Vec<i32> = COUNTERS
        .iter()
        .filter(|entry| entry.name.starts_with("cluster."))
        .map(|entry| entry.id.0)
        .collect();
    let mut members: Vec<MemberCounters> = Vec::new();
    for id in 0..reader.capacity() {
        let Some(fields) = reader.record(id).ok().flatten() else {
            continue;
        };
        let type_id = fields.type_id();
        if !cluster_ids.contains(&type_id) {
            continue;
        }
        let Ok(value) = reader.read_acquire(id) else {
            continue;
        };
        let mut key = [0u8; 4];
        if let Some(head) = fields.key().get(..4) {
            key.copy_from_slice(head);
        }
        let member_id = i32::from_le_bytes(key);
        let index = match members.iter().position(|member| member.id == member_id) {
            Some(index) => index,
            None => {
                members.push(MemberCounters {
                    id: member_id,
                    ..MemberCounters::default()
                });
                members.len() - 1
            }
        };
        let Some(member) = members.get_mut(index) else {
            continue;
        };
        if type_id == CLUSTER_ROLE.0 {
            member.role = Some(value);
        } else if type_id == CLUSTER_ELECTION_STATE.0 {
            member.election_state = Some(value);
        } else if type_id == CLUSTER_LEADERSHIP_TERM_ID.0 {
            member.term = Some(value);
        } else if type_id == CLUSTER_APPEND_POSITION.0 {
            member.append_position = Some(value);
        } else if type_id == CLUSTER_COMMIT_POSITION.0 {
            member.commit_position = Some(value);
        } else if type_id == CLUSTER_ELECTIONS_FAILED.0 {
            member.elections_failed = value;
        } else if type_id == CLUSTER_JOIN_POSITION_MISMATCHES.0 {
            member.join_position_mismatches = value;
        } else if type_id == CLUSTER_LEADER_QUORUM_LOSSES.0 {
            member.leader_quorum_losses = value;
        } else if type_id == CLUSTER_APPOINTED_LEADER_BEHIND.0 {
            member.appointed_leader_behind = value;
        } else if let Some(count) = MEMBER_COUNTS
            .iter()
            .position(|counter| counter.0 == type_id)
            .and_then(|index| member.counts.get_mut(index))
        {
            *count = value;
        }
    }
    if members.is_empty() {
        return 0;
    }
    members.sort_by_key(|member| member.id);
    for member in &members {
        member_findings(member, cluster, findings);
    }
    // The six of `member_findings`, and one for each of the member's counts.
    6 + MEMBER_COUNTS.len()
}

/// What `cluster-no-leader` says of member `id`, its election in `state`. The appointed
/// leader canvasses as a follower until it leads (`spec/cluster.md`, "The appointed
/// leader"), so a member with no leader may be the one the others wait for: with the
/// members string, doctor says which it is and where to look next; without it, both.
fn no_leader_message(id: i32, state: i64, cluster: Option<&Cluster>) -> String {
    let startup = STARTUP_CANVASS_TIMEOUT_NS / 1_000_000_000;
    let members = MEMBERS_KEY.env;
    let waits_for = |quorum: &str| {
        format!(
            "it leads once every other member's canvass position has reached it, or once \
             {quorum} has, itself among them, after the {startup} s startup canvass timeout"
        )
    };
    let other_hosts = format!(
        "a member it has not heard is not running, or its canvass positions do not reach \
         this member's consensus endpoint in {members}: run doctor on each other member's \
         host and check that it reaches that endpoint"
    );
    let leader_away = format!(
        "is not running, is not reached on the consensus endpoints, or waits up to {startup} \
         s for a member that has not started"
    );
    match cluster.filter(|cluster| cluster.ids().contains(&id)) {
        Some(cluster) if cluster.appointed_leader() == id => format!(
            "member {id} is the appointed leader, the member with the lowest id in {members}, \
             and leads no term yet: its election is in {}, and {}; {other_hosts}",
            election_state_name(state),
            waits_for(&format!(
                "a quorum, {} of {},",
                quorum(cluster.count),
                cluster.count
            ))
        ),
        Some(cluster) => {
            let leader = cluster.appointed_leader();
            format!(
                "member {id} follows no leader: its election is in {}. The appointed leader, \
                 member {leader}, the member with the lowest id in {members}, {leader_away}; \
                 run doctor on member {leader}'s host",
                election_state_name(state)
            )
        }
        None => format!(
            "member {id} follows no leader: its election is in {}. The appointed leader is \
             the member with the lowest id in {members}, which doctor was not given with \
             this member in it; set it and {} as `cavalry run` has them, and doctor says \
             which member that is. If it is another member, it {leader_away}: run doctor on \
             its host. If it is this member, {}; {other_hosts}",
            election_state_name(state),
            MEMBER_ID_KEY.env,
            waits_for("a quorum")
        ),
    }
}

/// The findings of one cluster member's counters.
fn member_findings(
    member: &MemberCounters,
    cluster: Option<&Cluster>,
    findings: &mut Vec<Finding>,
) {
    let id = member.id;
    let leads = member.role == Some(2);
    let state = member.election_state.unwrap_or(0);
    let closed = state == 17;
    let term = member.term.unwrap_or(-1);
    let append = member.append_position.unwrap_or(0);
    let commit = member.commit_position.unwrap_or(0);
    findings.push(Finding {
        check: "cluster-member",
        level: Level::Note,
        message: format!(
            "this driver is cluster member {id}, {} in term {term}, election {}: append \
             position {append}, commit position {commit}",
            if leads { "the leader" } else { "a follower" },
            election_state_name(state)
        ),
        doc: DOC_CLUSTER,
    });
    // The appointed leader counts a member ahead of it in canvass, where it then does not
    // lead, and in leader ready, where it leads and does not close its election
    // (`spec/cluster.md`, "The appointed leader"): either way its election stays open.
    if member.appointed_leader_behind > 0 && !closed {
        let stalled = if leads {
            format!(
                "so it leads term {term} and does not close its election, which stays in {}, \
                 and appends and commits nothing of the term",
                election_state_name(state)
            )
        } else {
            "so it does not lead".to_string()
        };
        findings.push(Finding {
            check: "cluster-leader-behind",
            level: Level::Fail,
            message: format!(
                "member {id} is the cluster's appointed leader and found {} member(s) holding \
                 a log ahead of its own, {stalled}: its log directory was lost or replaced \
                 while the others kept theirs. Start the cluster again as a new cluster, \
                 every member on a new log directory, and keep the old directories for what \
                 they hold",
                member.appointed_leader_behind
            ),
            doc: DOC_CLUSTER,
        });
    } else if !leads && !closed && member.elections_failed + member.join_position_mismatches > 0 {
        findings.push(Finding {
            check: "cluster-joined-late",
            level: Level::Warn,
            message: format!(
                "member {id} cannot join its leader's log: {} of its elections failed, {} of \
                 them on an image that joined past its append position. It started after \
                 the cluster's first entry, or lost what it held, and this version has no \
                 catch-up: the cluster runs without it. To have it back, start the cluster \
                 again as a new cluster, every member on a new log directory",
                member.elections_failed, member.join_position_mismatches
            ),
            doc: DOC_CLUSTER,
        });
    } else if !leads && !closed {
        findings.push(Finding {
            check: "cluster-no-leader",
            level: Level::Warn,
            message: no_leader_message(id, state, cluster),
            doc: DOC_CLUSTER,
        });
    }
    if leads && member.leader_quorum_losses > 0 {
        findings.push(Finding {
            check: "cluster-quorum-lost",
            level: Level::Warn,
            message: format!(
                "the leader, member {id}, has lost its quorum {} time(s): fewer than a \
                 majority of the members reported within 10 s, and nothing is committed \
                 while that lasts. Check that a majority of the members in {} run and reach \
                 this member's consensus endpoint",
                member.leader_quorum_losses, MEMBERS_KEY.env
            ),
            doc: DOC_CLUSTER,
        });
    }
    if leads && closed && commit < append {
        findings.push(Finding {
            check: "cluster-commit-waits",
            level: Level::Note,
            message: format!(
                "the leader, member {id}, has appended to position {append} and a quorum has \
                 recorded to {commit}: {} bytes wait for a quorum. A member's recorder syncs \
                 once a second, so up to a second of entries waits at any moment; if the \
                 commit position stays while the append position moves, a quorum of members \
                 is not reporting: run doctor on each member's host",
                append - commit
            ),
            doc: DOC_CLUSTER,
        });
    }
    for (counter, value) in MEMBER_COUNTS.iter().zip(member.counts) {
        let Some(def) = COUNTERS.iter().find(|def| def.id == *counter) else {
            continue;
        };
        if value > 0 {
            findings.push(Finding {
                check: def.name,
                level: Level::Warn,
                message: format!("member {id}'s {} is {value}: {}", def.name, def.doctor),
                doc: DOC_CLUSTER,
            });
        }
    }
}

/// One endpoint, send or receive, as the counters file shows it: the `channel.status`
/// record's key and label, and the endpoint's other counters joined by the key. A send
/// endpoint carries publications and no image, a receive endpoint images and no
/// publication (`spec/udp.md`, "Counters").
struct EndpointCounters {
    id: i64,
    channel: String,
    status: i64,
    sndbuf: Option<i64>,
    rcvbuf: Option<i64>,
    publications: Option<i64>,
    images: Option<i64>,
    /// `channel.destinations`: present on a driver of ADR 0053's version, 0 off a
    /// control channel.
    destinations: Option<i64>,
}

impl EndpointCounters {
    /// What the endpoint is, from its counts. A send endpoint carries publications and
    /// closes when it has none; a receive endpoint carries images, or none while it
    /// waits for a setup frame, and is the only kind that is active with nothing on it.
    /// One that is closing with nothing on it could be either, and is called an
    /// endpoint.
    fn kind(&self) -> &'static str {
        if self.publications.unwrap_or(0) > 0 {
            "send endpoint"
        } else if self.images.unwrap_or(0) > 0 || (self.status == 1 && self.images.is_some()) {
            "receive endpoint"
        } else {
            "endpoint"
        }
    }
}

/// One image as the counters file shows it: the key of its `image.hwm_position` record,
/// its label's three ids, and the two positions joined by the key.
struct ImageCounters {
    key: [u8; 16],
    registration_id: i64,
    session_id: i32,
    stream_id: i32,
    hwm: Option<i64>,
    rebuild: Option<i64>,
}

/// The `cavalry:udp` checks ADR 0039 names, from the counters file alone (ADR 0041,
/// ADR 0042): a publication no receiver has answered, whose `sender.limit` is 0 while
/// the driver runs, a control channel's with no destination told apart; an endpoint's
/// socket buffer the host capped, a send buffer below [`SNDBUF_LEAST`] or a receive
/// buffer below [`RCVBUF_EXPECTED`], each finding naming the sysctl of the platform
/// `doctor` runs on, which is the driver's host; loss on an image, whose
/// `image.hwm_position` runs ahead of its `image.rebuild_position`; a receive endpoint
/// with no image, whose subscriptions wait for a setup frame that has not come; and
/// ADR 0053's three, a `min` publication with fewer receivers answering than its
/// endpoint has destinations, read from the gauges whose label names `fc=min`, a
/// receiver dropped from a `min` table and a table that was full. Every endpoint is
/// listed as a note, as recordings are, with its status,
/// its publications or images, its destinations on a control channel, and its buffers.
fn udp_checks(reader: &CountersReader, running: bool, findings: &mut Vec<Finding>) -> usize {
    if !running {
        return 0;
    }
    let capacity = reader.capacity();
    let mut endpoints: Vec<EndpointCounters> = Vec::new();
    let mut buffers: Vec<(i64, i32, i64)> = Vec::new();
    let mut unanswered: Vec<(i64, i32, i32, i64)> = Vec::new();
    let mut images: Vec<ImageCounters> = Vec::new();
    let mut positions: Vec<([u8; 16], i32, i64)> = Vec::new();
    // Every `sender.flow_control_receivers` gauge of a `min` publication: its
    // publication's ids, its endpoint's registration id and its value; a `max`
    // publication's, which the sender holds at 0, is not read (ADR 0053).
    let mut receivers: Vec<(i64, i32, i32, i64, i64)> = Vec::new();
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
                images: None,
                destinations: None,
            });
        } else if type_id == CHANNEL_SOCKET_SNDBUF.0
            || type_id == CHANNEL_SOCKET_RCVBUF.0
            || type_id == CHANNEL_PUBLICATIONS.0
            || type_id == CHANNEL_IMAGES.0
            || type_id == CHANNEL_DESTINATIONS.0
        {
            buffers.push((endpoint_id(fields.key()), type_id, value));
        } else if type_id == SENDER_FLOW_CONTROL_RECEIVERS.0 {
            // The label: the name, the correlation id, the session id, the stream id
            // and the endpoint's registration id, as sender.limit's, then the
            // strategy, `fc=max` or `fc=min`; only a `min` publication keeps a table.
            let mut words = fields.label().split_whitespace().skip(1);
            let (Some(correlation), Some(session), Some(stream), Some(endpoint), Some("fc=min")) = (
                words.next(),
                words.next(),
                words.next(),
                words.next(),
                words.next(),
            ) else {
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
            receivers.push((correlation_id, session_id, stream_id, endpoint_id, value));
        } else if type_id == IMAGE_HWM_POSITION.0 || type_id == IMAGE_REBUILD_POSITION.0 {
            let mut key = [0u8; 16];
            if let Some(head) = fields.key().get(..16) {
                key.copy_from_slice(head);
            }
            positions.push((key, type_id, value));
            if type_id == IMAGE_HWM_POSITION.0 {
                // The label: the name, the registration id, the session id, the stream id.
                let mut words = fields.label().split_whitespace().skip(1);
                let (Some(registration), Some(session), Some(stream)) =
                    (words.next(), words.next(), words.next())
                else {
                    continue;
                };
                let (Ok(registration_id), Ok(session_id), Ok(stream_id)) = (
                    registration.parse::<i64>(),
                    session.parse::<i32>(),
                    stream.parse::<i32>(),
                ) else {
                    continue;
                };
                images.push(ImageCounters {
                    key,
                    registration_id,
                    session_id,
                    stream_id,
                    hwm: None,
                    rebuild: None,
                });
            }
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
            } else if type_id == CHANNEL_PUBLICATIONS.0 {
                endpoint.publications = Some(value);
            } else if type_id == CHANNEL_DESTINATIONS.0 {
                endpoint.destinations = Some(value);
            } else {
                endpoint.images = Some(value);
            }
        }
    }
    for (key, type_id, value) in positions {
        if let Some(image) = images.iter_mut().find(|image| image.key == key) {
            if type_id == IMAGE_HWM_POSITION.0 {
                image.hwm = Some(value);
            } else {
                image.rebuild = Some(value);
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
    let destinations_of = |id: i64| {
        endpoints
            .iter()
            .find(|endpoint| endpoint.id == id)
            .and_then(|endpoint| endpoint.destinations)
    };
    let is_control = |id: i64| {
        endpoints
            .iter()
            .find(|endpoint| endpoint.id == id)
            .is_some_and(|endpoint| endpoint.channel.contains("control="))
    };
    for (correlation_id, session_id, stream_id, endpoint_id) in &unanswered {
        let channel = channel_of(*endpoint_id);
        let message = if is_control(*endpoint_id) && destinations_of(*endpoint_id) == Some(0) {
            format!(
                "publication {correlation_id} on stream {stream_id} (session {session_id}) \
                 sends on the control channel {channel}, which has no destination: nothing \
                 goes on the wire, its sender limit is 0 and the publisher reads not \
                 connected; add a destination with add destination on the publication, \
                 cavalry:udp?endpoint=<host>:<port> of a host whose driver subscribes to \
                 stream {stream_id} on that port"
            )
        } else if is_control(*endpoint_id) {
            format!(
                "publication {correlation_id} on stream {stream_id} (session {session_id}) \
                 sends on the control channel {channel} to {} destination(s) and none has \
                 answered: its sender limit is 0, its setup frames go unanswered every 100 \
                 ms and the publisher reads not connected; check that a subscription on \
                 stream {stream_id} exists on each destination's host, that its driver \
                 receives on that port and can reach the control address, and that both \
                 drivers run the same version; a rising sender.send_failed is a destination \
                 the control socket cannot send to",
                destinations_of(*endpoint_id).unwrap_or(0)
            )
        } else {
            format!(
                "publication {correlation_id} on stream {stream_id} (session {session_id}) \
                 sends on {channel} and no receiver has answered: its sender limit is 0, its \
                 setup frames go unanswered every 100 ms and the publisher reads not \
                 connected; check that a subscription on stream {stream_id} exists on the \
                 endpoint's host, that its driver receives on that port, and that both \
                 drivers run the same version"
            )
        };
        findings.push(Finding {
            check: "udp-publication-not-connected",
            level: Level::Warn,
            message,
            doc: DOC_UDP_SENDER,
        });
    }
    // A `min` publication with fewer receivers in its table than its endpoint has
    // destinations: a destination that is not answering within 5 s, which holds the
    // stream back until the timeout drops it and then runs without it (ADR 0053).
    for (correlation_id, session_id, stream_id, endpoint_id, answering) in &receivers {
        if let Some(destinations) = destinations_of(*endpoint_id)
            && destinations > 0
            && *answering < destinations
        {
            findings.push(Finding {
                check: "udp-flow-control-receivers",
                level: Level::Warn,
                message: format!(
                    "publication {correlation_id} on stream {stream_id} (session \
                     {session_id}) sends under fc=min on {} to {destinations} destination(s) \
                     and {answering} answer within 5 s: the sender limit follows the slowest \
                     receiver that answers, and a destination that stopped answering held the \
                     stream back for the flow control receiver timeout before it was dropped \
                     (sender.flow_control_receivers_timed_out); check that destination's \
                     driver and its subscription on stream {stream_id}, or remove the \
                     destination with remove destination",
                    channel_of(*endpoint_id)
                ),
                doc: DOC_UDP_SENDER,
            });
        }
    }
    // The counter counts a receiver dropped for the timeout alone; one whose stream
    // ended, as every receiver's does after a clean close, is not in it (ADR 0054).
    if let Some(timed_out) = read_static(
        reader,
        "sender.flow_control_receivers_timed_out",
        SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT,
        findings,
    ) && timed_out > 0
    {
        findings.push(Finding {
            check: "udp-flow-control-receiver-timed-out",
            level: Level::Warn,
            message: format!(
                "sender.flow_control_receivers_timed_out is {timed_out}: that many times a \
                 receiver of a publication under fc=min sent no status message for 5 s and \
                 was dropped from the publication's table, each time after holding the \
                 stream back for the timeout; a receiver whose stream ended is not counted, \
                 so this is a destination whose driver stopped, lost its subscription or \
                 lost its route: find it by the publication whose \
                 sender.flow_control_receivers is below its endpoint's \
                 channel.destinations, check its driver, or remove the destination"
            ),
            doc: DOC_UDP_SENDER,
        });
    }
    if let Some(full) = read_static(
        reader,
        "sender.flow_control_receivers_full",
        SENDER_FLOW_CONTROL_RECEIVERS_FULL,
        findings,
    ) && full > 0
    {
        findings.push(Finding {
            check: "udp-flow-control-receivers-full",
            level: Level::Warn,
            message: format!(
                "sender.flow_control_receivers_full is {full}: that many status messages \
                 came from a receiver a publication's full table of 64 did not hold, so the \
                 sender limit does not follow that receiver; 64 destinations never produce \
                 this, but a receiver that restarts draws a new receiver id each time, so a \
                 flapping destination fills the table: find it from its driver's \
                 image.created, and let the dropped ids time out"
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
        let kind = endpoint.kind();
        if let Some(sndbuf) = endpoint.sndbuf
            && sndbuf < SNDBUF_LEAST
            && kind != "receive endpoint"
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
            let cost = if kind == "receive endpoint" {
                "data datagrams dropped under load, each a gap to NAK"
            } else {
                "status messages and NAKs dropped under load"
            };
            findings.push(Finding {
                check: "udp-socket-buffers",
                level: Level::Warn,
                message: format!(
                    "{kind} {}: the socket's receive buffer reads back {rcvbuf} bytes, \
                     below the {RCVBUF_EXPECTED} a host that granted the {SOCKET_RCVBUF_LENGTH} \
                     asked reads back, so the host capped the ask: {RCVBUF_REMEDY}, {reopen}; \
                     until then expect {cost}",
                    endpoint.channel
                ),
                doc: DOC_UDP_VALUES,
            });
        }
        // A receive endpoint whose subscriptions have no image: no sender's setup
        // frame has reached this port for a stream they want, or the stream they had
        // ended and its image went while they stayed (ADR 0042). image.created and
        // image.ended are driver-wide, every receive endpoint's images since the
        // driver started, and the file holds no count per endpoint, so they settle
        // the cause on a driver with one receive endpoint and no earlier stream alone;
        // the finding says so and keeps the checks for a stream that never came.
        if kind == "receive endpoint" && endpoint.status == 1 && endpoint.images == Some(0) {
            let created = read_static(reader, "image.created", IMAGE_CREATED, findings);
            let ended = read_static(reader, "image.ended", IMAGE_ENDED, findings);
            // A count read_static could not read has its own Note or Fail, and is
            // never printed as 0.
            let tell = match (created, ended) {
                (Some(created), Some(ended)) => format!(
                    "image.created {created} and image.ended {ended} are driver-wide, every \
                     receive endpoint's images since the driver started: image.created 0 is a \
                     stream that has reached no receive endpoint on this driver, and \
                     image.created above 0 with image.ended at it is a stream that came and \
                     ended on a receive endpoint here, which one the counters file does not say"
                ),
                _ => "image.created and image.ended, the driver-wide counts of every receive \
                      endpoint's images that would say which, cannot be read from this file, \
                      as their own finding says"
                    .to_string(),
            };
            findings.push(Finding {
                check: "udp-subscription-no-image",
                level: Level::Warn,
                message: format!(
                    "receive endpoint {}: no image: either no sender's setup frame has reached \
                     this port for a stream a subscription here wants, or the stream it had \
                     ended and its image went while the subscription stayed, so the \
                     subscriptions read nothing; {tell}; with more than one receive endpoint, \
                     or a stream that ended before, the checks for a stream that never came \
                     still apply to this endpoint: that the sender's channel names this host \
                     and port and its stream id matches, that the port is open between the \
                     hosts, and that both drivers run the same version; a rising \
                     receiver.setups_unwanted is a sender on another stream id, and a rising \
                     receiver.dropped_frames a peer of another version",
                    endpoint.channel
                ),
                doc: DOC_UDP_RECEIVER,
            });
        }
        findings.push(Finding {
            check: "udp-endpoint",
            level: Level::Note,
            message: format!(
                "{kind} {}: status {}, {} publication(s), {} image(s){}, send buffer {} \
                 bytes, receive buffer {} bytes",
                endpoint.channel,
                match endpoint.status {
                    0 => "initializing".to_string(),
                    1 => "active".to_string(),
                    2 => "closing".to_string(),
                    -1 => "errored".to_string(),
                    other => other.to_string(),
                },
                endpoint.publications.unwrap_or(0),
                endpoint.images.unwrap_or(0),
                if endpoint.channel.contains("control=") {
                    format!(", {} destination(s)", endpoint.destinations.unwrap_or(0))
                } else {
                    String::new()
                },
                endpoint.sndbuf.unwrap_or(0),
                endpoint.rcvbuf.unwrap_or(0)
            ),
            doc: DOC_UDP_VALUES,
        });
    }
    let checks = 7 + interface_mtu_checks(&endpoints, findings);
    // Loss on an image: the high-water mark past the rebuild position is a gap the
    // receiver is NAKing, or one the sender no longer answers (ADR 0042).
    for image in &images {
        if let (Some(hwm), Some(rebuild)) = (image.hwm, image.rebuild)
            && hwm > rebuild
        {
            findings.push(Finding {
                check: "udp-image-loss",
                level: Level::Warn,
                message: format!(
                    "image {} on stream {} (session {}): its high-water mark {hwm} is {} bytes \
                     past its rebuild position {rebuild}: a gap on the wire the receiver NAKs \
                     every 100 µs until the sender mends it (receiver.gaps_detected, \
                     receiver.naks_sent here; sender.retransmits_sent on the sending host); a \
                     rebuild position that stands still is a gap the sender no longer holds \
                     (sender.naks_ignored there), and the image times out after 10 s; look at \
                     the link and at the socket buffers on both hosts",
                    image.registration_id,
                    image.stream_id,
                    image.session_id,
                    hwm - rebuild
                ),
                doc: DOC_UDP_RECEIVER,
            });
        }
    }
    checks
}

/// The interface MTU check ADR 0039 names (ADR 0044), on Linux: every active endpoint
/// whose address routes over an interface with an MTU below [`INTERFACE_MTU_LEAST`] is
/// a warning, from the main routing table and the interface's `mtu` file. The check
/// counts as run on Linux whether or not the table can be read, as the other host
/// checks count theirs; elsewhere there is nothing to read, and the check does not run.
fn interface_mtu_checks(endpoints: &[EndpointCounters], findings: &mut Vec<Finding>) -> usize {
    if !cfg!(target_os = "linux") {
        return 0;
    }
    if let Some(routes) = routing_table() {
        interface_mtu_findings(endpoints, &routes, &interface_mtu, findings);
    }
    1
}

/// The main routing table of a Linux host as `/proc/net/route` prints it, one route per
/// line after a header, the interface, the destination and the mask as hexadecimal words
/// in the host's byte order, the flags with bit 0 for a route that is up and bit 9 for
/// one that rejects, and the metric; `None` when it cannot be read or the host is not
/// Linux.
fn routing_table() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::fs::read_to_string("/proc/net/route").ok()
}

/// An interface's MTU on a Linux host, from its `mtu` file under `/sys/class/net`.
fn interface_mtu(interface: &str) -> Option<u32> {
    std::fs::read_to_string(format!("/sys/class/net/{interface}/mtu"))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
}

/// The findings of the interface MTU check over `routes`, the main routing table as
/// Linux prints it, and `mtu_of`, an interface's MTU: for each active endpoint whose
/// channel names an IPv4 address, the interface its datagrams go through, the one that
/// holds the channel's `interface=` address when the channel names one and the one the
/// endpoint's address routes over otherwise, and a warning when that interface's MTU is
/// below [`INTERFACE_MTU_LEAST`]. An IPv6 endpoint, an endpoint bound to every address
/// and one that is closing are left alone, and the pages say so. Pure over its inputs,
/// so the tests hold a table of their own.
fn interface_mtu_findings(
    endpoints: &[EndpointCounters],
    routes: &str,
    mtu_of: &dyn Fn(&str) -> Option<u32>,
    findings: &mut Vec<Finding>,
) {
    for endpoint in endpoints.iter().filter(|endpoint| endpoint.status == 1) {
        let Some((address, interface_address)) = endpoint_addresses(&endpoint.channel) else {
            continue;
        };
        let Some(interface) = route_interface(routes, interface_address.unwrap_or(address)) else {
            continue;
        };
        let Some(mtu) = mtu_of(&interface) else {
            continue;
        };
        if mtu < INTERFACE_MTU_LEAST {
            let kind = endpoint.kind();
            let direction = match kind {
                "send endpoint" => "leave through",
                "receive endpoint" => "arrive through",
                _ => "go through",
            };
            findings.push(Finding {
                check: "udp-interface-mtu",
                level: Level::Warn,
                message: format!(
                    "{kind} {}: its datagrams {direction} {interface}, whose MTU is {mtu} bytes, \
                     below the {INTERFACE_MTU_LEAST} a datagram of the stream's MTU, \
                     {MTU_LENGTH_DEFAULT} bytes, needs with its IPv4 and UDP headers; a datagram \
                     longer than the interface's MTU is fragmented by the kernel, and one lost \
                     fragment loses the whole datagram, or it is dropped where fragmentation is \
                     off; raise the interface's MTU to {INTERFACE_MTU_LEAST} or more, or route \
                     the endpoint over an interface that carries it; the stream's MTU has no key \
                     in this version",
                    endpoint.channel
                ),
                doc: DOC_UDP_VALUES,
            });
        }
    }
}

/// The IPv4 address a `cavalry:udp` channel's endpoint names, and the address of its
/// `interface=` option when it has one, as the counters file's labels hold them: the
/// driver resolves a name once, as the command is handled, and writes the resolved
/// channel into the label (`spec/udp.md`, "The channel URI"), so no name is looked up
/// here. `None` for an IPv6 endpoint or an unspecified address.
fn endpoint_addresses(channel: &str) -> Option<(Ipv4Addr, Option<Ipv4Addr>)> {
    let ipv4 = |text: &str| -> Option<Ipv4Addr> {
        let host = match text.rsplit_once(':') {
            Some((host, port)) if port.parse::<u16>().is_ok() => host,
            _ => text,
        };
        host.parse::<Ipv4Addr>().ok()
    };
    let mut parameters = channel.strip_prefix("cavalry:udp?endpoint=")?.split('|');
    let address = ipv4(parameters.next()?).filter(|address| !address.is_unspecified())?;
    let interface = parameters
        .find_map(|parameter| parameter.strip_prefix("interface="))
        .and_then(ipv4)
        .filter(|interface| !interface.is_unspecified());
    Some((address, interface))
}

/// The interface a datagram to `destination` leaves through, from `routes`, the main
/// routing table as `/proc/net/route` prints it: among the routes that are up and do
/// not reject, the one with the longest mask that covers the destination, and the lowest
/// metric among those of equal length. A loopback destination is `lo`, which the main
/// table does not list.
fn route_interface(routes: &str, destination: Ipv4Addr) -> Option<String> {
    const UP: u32 = 0x1;
    const REJECT: u32 = 0x200;
    if destination.is_loopback() {
        return Some("lo".to_string());
    }
    let word = |field: &str| u32::from_str_radix(field, 16).ok();
    let mut best: Option<(u32, u32, String)> = None;
    for line in routes.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (Some(interface), Some(dest), _gateway, Some(flags), _, _, Some(metric), Some(mask)) = (
            fields.next(),
            fields.next().and_then(word),
            fields.next(),
            fields.next().and_then(word),
            fields.next(),
            fields.next(),
            fields.next().and_then(|field| field.parse::<u32>().ok()),
            fields.next().and_then(word),
        ) else {
            continue;
        };
        if flags & UP == 0 || flags & REJECT != 0 {
            continue;
        }
        // The words are the addresses as the host stores them, so their bytes in the
        // host's order are the address in network order.
        let dest = Ipv4Addr::from(dest.to_ne_bytes());
        let mask = Ipv4Addr::from(mask.to_ne_bytes());
        if u32::from(destination) & u32::from(mask) != u32::from(dest) {
            continue;
        }
        let length = u32::from(mask).count_ones();
        let better = best
            .as_ref()
            .is_none_or(|(bits, cost, _)| length > *bits || (length == *bits && metric < *cost));
        if better {
            best = Some((length, metric, interface.to_string()));
        }
    }
    best.map(|(_, _, interface)| interface)
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

/// [`examine_with`] for a configuration, with the checks of the keys that name a
/// cluster: `cluster.members` and `cluster.member.id` set together, the members string
/// one the cluster protocol takes, the member id one of the string's, and every
/// endpoint of the string a name that resolves (`cluster-members`,
/// `cluster-member-id`; ADR 0060). Neither key set is no cluster and no finding. A
/// members string the protocol takes also names the appointed leader to the checks of
/// the driver's member, so that `cluster-no-leader` says whether it is this member.
pub fn examine_config(config: &Config) -> Report {
    let cluster = Cluster::of(config.cluster_members.as_deref());
    let mut report = examine_in(&config.dir, config.log_dir.as_deref(), cluster.as_ref());
    let mut findings = std::mem::take(&mut report.findings);
    let checks = report.checks
        + cluster_key_checks(
            config.cluster_members.as_deref(),
            config.cluster_member_id,
            &mut findings,
        );
    finish(&config.dir, findings, checks)
}

/// The checks of the `cluster.members` and `cluster.member.id` keys: what `cavalry run`
/// would refuse or a member could not reach, said before the member is started. Returns
/// how many checks ran: none for a driver that is in no cluster.
fn cluster_key_checks(
    members: Option<&str>,
    member_id: Option<i32>,
    findings: &mut Vec<Finding>,
) -> usize {
    let (members, member_id) = match (members, member_id) {
        (None, None) => return 0,
        (Some(_), None) => {
            findings.push(Finding {
                check: "cluster-member-id",
                level: Level::Fail,
                message: format!(
                    "{} is set and {} is unset or -1, no member, so `cavalry run` cannot \
                     tell which member this host is and starts none: set {} to this host's \
                     id in the members string, or unset {} for a driver in no cluster",
                    MEMBERS_KEY.env, MEMBER_ID_KEY.env, MEMBER_ID_KEY.env, MEMBERS_KEY.env
                ),
                doc: MEMBER_ID_KEY.anchor,
            });
            return 2;
        }
        (None, Some(id)) => {
            findings.push(Finding {
                check: "cluster-members",
                level: Level::Fail,
                message: format!(
                    "{} is {id} and {} is not set, so there is no cluster for member {id} to \
                     be in: set {} to the cluster's members string, the same on every \
                     member, or unset {} for a driver in no cluster",
                    MEMBER_ID_KEY.env, MEMBERS_KEY.env, MEMBERS_KEY.env, MEMBER_ID_KEY.env
                ),
                doc: MEMBERS_KEY.anchor,
            });
            return 2;
        }
        (Some(members), Some(member_id)) => (members, member_id),
    };
    let mut parsed = [EMPTY_MEMBER; MAX_MEMBERS];
    let count = match parse_members(members.as_bytes(), &mut parsed, None) {
        Ok(count) => count,
        Err(error) => {
            findings.push(Finding {
                check: "cluster-members",
                level: Level::Fail,
                message: format!(
                    "{} is refused ({}): {error}; write each member as \
                     `id,ingress,consensus,log,catchup,archive`, an id from 0 and five \
                     `host:port` endpoints, members separated by `|`, and give every member \
                     the same string",
                    MEMBERS_KEY.env,
                    error.reason()
                ),
                doc: MEMBERS_KEY.anchor,
            });
            return 2;
        }
    };
    let parsed = parsed.get(..count).unwrap_or(&[]);
    if !parsed.iter().any(|member| member.id == member_id) {
        let ids: Vec<String> = parsed.iter().map(|member| member.id.to_string()).collect();
        findings.push(Finding {
            check: "cluster-member-id",
            level: Level::Fail,
            message: format!(
                "{} is {member_id}, which is not a member of {}, whose ids are {}: set it to \
                 this host's id in the string",
                MEMBER_ID_KEY.env,
                MEMBERS_KEY.env,
                ids.join(", ")
            ),
            doc: MEMBER_ID_KEY.anchor,
        });
    }
    for member in parsed {
        for (name, endpoint) in [
            ("ingress", member.ingress),
            ("consensus", member.consensus),
            ("log", member.log),
            ("catchup", member.catchup),
            ("archive", member.archive),
        ] {
            let host = String::from_utf8_lossy(endpoint.host);
            let resolved = (host.as_ref(), endpoint.port)
                .to_socket_addrs()
                .map(|mut addresses| addresses.next().is_some());
            if !matches!(resolved, Ok(true)) {
                findings.push(Finding {
                    check: "cluster-members",
                    level: Level::Fail,
                    message: format!(
                        "member {}'s {name} endpoint in {}, {host}:{}, resolves to no address \
                         on this host: no member can reach it there; correct the host name in \
                         the string on every member, or make the name resolve",
                        member.id, MEMBERS_KEY.env, endpoint.port
                    ),
                    doc: MEMBERS_KEY.anchor,
                });
            }
        }
    }
    2
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

    /// ADR 0053's checks from the counters file alone: a control channel with no
    /// destination is told apart in the not-connected warning, a `min` publication
    /// with fewer receivers answering than its endpoint has destinations warns naming
    /// both counts, a dropped receiver and a full table warn from their static
    /// counters, and the endpoint's note counts its destinations.
    #[test]
    fn a_control_channel_without_destinations_and_a_min_publication_short_of_receivers_warn() {
        let dir = TestDir::new("udp-mdc");
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let endpoint_id = 9i64;
        let channel = "cavalry:udp?control=10.0.0.1:40456|control-mode=manual";
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
        sndbuf.set(212_992);
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
        let destinations = counters
            .allocate(
                CHANNEL_DESTINATIONS,
                &endpoint_key,
                &format!("channel.destinations {channel}"),
            )
            .expect("room");
        destinations.set(0);
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&7i64.to_le_bytes());
        key[8..12].copy_from_slice(&5i32.to_le_bytes());
        key[12..].copy_from_slice(&1001i32.to_le_bytes());
        let limit = counters
            .allocate(SENDER_LIMIT, &key, "sender.limit 7 5 1001 9")
            .expect("room");
        limit.set(0);
        let receivers = counters
            .allocate(
                SENDER_FLOW_CONTROL_RECEIVERS,
                &key,
                "sender.flow_control_receivers 7 5 1001 9 fc=min",
            )
            .expect("room");
        receivers.set(0);

        // No destination: the not-connected warning says to add one.
        let report = examine(&dir.0);
        let found = finding(&report, "udp-publication-not-connected");
        assert_eq!(found.level, Level::Warn);
        assert!(
            found.message.contains("has no destination"),
            "{}",
            found.message
        );
        assert!(
            found.message.contains("add destination"),
            "{}",
            found.message
        );
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "udp-flow-control-receivers"),
            "{report}"
        );
        let note = finding(&report, "udp-endpoint");
        assert!(
            note.message.contains("0 destination(s)"),
            "{}",
            note.message
        );

        // Two destinations and one receiver answering, the publication connected: the
        // min warning names both counts.
        destinations.set(2);
        limit.store_release(131_072);
        receivers.set(1);
        let report = examine(&dir.0);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "udp-publication-not-connected"),
            "{report}"
        );
        let found = finding(&report, "udp-flow-control-receivers");
        assert_eq!(found.level, Level::Warn);
        assert!(
            found.message.contains("2 destination(s)"),
            "{}",
            found.message
        );
        assert!(found.message.contains("1 answer"), "{}", found.message);
        assert_eq!(found.doc, "docs/reference/udp.md#the-sender");
        let note = finding(&report, "udp-endpoint");
        assert!(
            note.message.contains("2 destination(s)"),
            "{}",
            note.message
        );

        // Both answer: no warning; the static counters warn on their own.
        receivers.set(2);
        counters
            .counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT.record())
            .expect("static")
            .increment();
        counters
            .counter(SENDER_FLOW_CONTROL_RECEIVERS_FULL.record())
            .expect("static")
            .add(3);
        let report = examine(&dir.0);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "udp-flow-control-receivers"),
            "{report}"
        );
        let found = finding(&report, "udp-flow-control-receiver-timed-out");
        assert_eq!(found.level, Level::Warn);
        assert!(found.message.contains("is 1"), "{}", found.message);
        let found = finding(&report, "udp-flow-control-receivers-full");
        assert_eq!(found.level, Level::Warn);
        assert!(found.message.contains("is 3"), "{}", found.message);
        assert!(report.ok(), "warnings do not fail: {report}");
    }

    /// A control channel's send endpoint in `counters` as the conductor allocates one:
    /// active, its buffers, one publication and `destinations` destinations.
    fn control_endpoint(counters: &mut CountersFile, endpoint_id: i64, destinations: i64) {
        let channel = "cavalry:udp?control=10.0.0.1:40456|control-mode=manual";
        let key = endpoint_id.to_le_bytes();
        for (id, name, value) in [
            (CHANNEL_STATUS, "channel.status", 1),
            (CHANNEL_SOCKET_SNDBUF, "channel.socket_sndbuf", 212_992),
            (
                CHANNEL_SOCKET_RCVBUF,
                "channel.socket_rcvbuf",
                RCVBUF_EXPECTED,
            ),
            (CHANNEL_PUBLICATIONS, "channel.publications", 1),
            (CHANNEL_DESTINATIONS, "channel.destinations", destinations),
        ] {
            counters
                .allocate(id, &key, &format!("{name} {channel}"))
                .expect("room")
                .set(value);
        }
    }

    /// A publication's `sender.limit` and `sender.flow_control_receivers` on stream
    /// 1001, session 5, labelled as the conductor labels them, `strategy` last.
    fn sender_gauges(
        counters: &mut CountersFile,
        correlation_id: i64,
        endpoint_id: i64,
        strategy: &str,
        (limit, receivers): (i64, i64),
    ) {
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&correlation_id.to_le_bytes());
        key[8..12].copy_from_slice(&5i32.to_le_bytes());
        key[12..].copy_from_slice(&1001i32.to_le_bytes());
        counters
            .allocate(
                SENDER_LIMIT,
                &key,
                &format!("sender.limit {correlation_id} 5 1001 {endpoint_id}"),
            )
            .expect("room")
            .set(limit);
        counters
            .allocate(
                SENDER_FLOW_CONTROL_RECEIVERS,
                &key,
                &format!(
                    "sender.flow_control_receivers {correlation_id} 5 1001 {endpoint_id} \
                     {strategy}"
                ),
            )
            .expect("room")
            .set(receivers);
    }

    /// A `max` publication keeps no table, and the sender holds its
    /// `sender.flow_control_receivers` at 0 (ADR 0053): `doctor` reads the strategy
    /// from the gauge's label, so a `max` publication on a control channel with two
    /// destinations, connected, gets no `udp-flow-control-receivers` warning, and a
    /// `min` one beside it with one receiver answering gets the one warning.
    #[test]
    fn a_max_publication_on_a_control_channel_is_not_read_as_short_of_receivers() {
        let dir = TestDir::new("udp-mdc-max");
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        control_endpoint(&mut counters, 9, 2);
        sender_gauges(&mut counters, 7, 9, "fc=max", (131_072, 0));
        let report = examine(&dir.0);
        for check in [
            "udp-flow-control-receivers",
            "udp-publication-not-connected",
        ] {
            assert!(
                !report.findings.iter().any(|f| f.check == check),
                "{check}: {report}"
            );
        }
        sender_gauges(&mut counters, 8, 9, "fc=min", (131_072, 1));
        let report = examine(&dir.0);
        let found: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.check == "udp-flow-control-receivers")
            .collect();
        assert_eq!(found.len(), 1, "{report}");
        assert!(
            found[0].message.contains("publication 8 on stream"),
            "{}",
            found[0].message
        );
    }

    /// A `min` publication on a control channel during the linger after a clean close
    /// (ADR 0056), its counters as the driver's
    /// `a_clean_close_of_a_min_publication_counts_no_receiver_timed_out` reads them
    /// before the linger ends: both destinations' receivers ended their streams and
    /// stay in the table, so the gauge reads the endpoint's 2 destinations, the limit
    /// is above 0 and nothing timed out, and `doctor` gives no `udp-` warning. A live
    /// `min` publication beside it with one of the two answering still warns, and the
    /// warning names it alone.
    #[test]
    fn a_min_publication_lingering_after_a_clean_close_leaves_doctor_with_no_warning() {
        let dir = TestDir::new("udp-mdc-min-linger");
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        control_endpoint(&mut counters, 9, 2);
        sender_gauges(&mut counters, 8, 9, "fc=min", (131_072, 2));
        // The udp checks: the test's directory may be on a disk, which another warns of.
        let report = examine(&dir.0);
        let warnings: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.level == Level::Warn && f.check.starts_with("udp-"))
            .collect();
        assert!(warnings.is_empty(), "{report}");
        sender_gauges(&mut counters, 10, 9, "fc=min", (131_072, 1));
        let report = examine(&dir.0);
        let found: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.check == "udp-flow-control-receivers")
            .collect();
        assert_eq!(found.len(), 1, "{report}");
        assert!(
            found[0].message.contains("publication 10 on stream"),
            "{}",
            found[0].message
        );
        assert!(
            found[0].message.contains("1 answer"),
            "{}",
            found[0].message
        );
    }

    /// A clean close of a `min` publication on a control channel (ADR 0054, ADR 0056):
    /// its receiver ended its stream, so the sender kept it in the table uncounted
    /// until the linger ended, and the close then freed the publication's counters.
    /// What the close leaves, the endpoint open for a `max` publication beside it to
    /// its one destination and both static counters at 0, gives `doctor` no warning.
    #[test]
    fn a_clean_close_of_a_min_publication_leaves_doctor_with_no_warning() {
        let dir = TestDir::new("udp-mdc-min-close");
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        control_endpoint(&mut counters, 9, 1);
        sender_gauges(&mut counters, 8, 9, "fc=max", (131_072, 0));
        // The udp checks: the test's directory may be on a disk, which another warns of.
        let report = examine(&dir.0);
        let warnings: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.level == Level::Warn && f.check.starts_with("udp-"))
            .collect();
        assert!(warnings.is_empty(), "{report}");
        // The counter the sweep used to raise on that close: now it means a timeout,
        // and the finding says so.
        counters
            .counter(SENDER_FLOW_CONTROL_RECEIVERS_TIMED_OUT.record())
            .expect("static")
            .increment();
        let report = examine(&dir.0);
        let found = finding(&report, "udp-flow-control-receiver-timed-out");
        assert!(
            found
                .message
                .contains("a receiver whose stream ended is not counted"),
            "{}",
            found.message
        );
        assert!(
            !found.message.contains("or ended its stream"),
            "{}",
            found.message
        );
    }

    /// ADR 0042's two `cavalry:udp` checks for the receiving side, from the counters
    /// file alone: a receive endpoint, one with images and no publication, that is
    /// active with no image is a subscription waiting for a setup frame that has not
    /// come; an image whose high-water mark is past its rebuild position is loss on the
    /// wire, named with the gap's bytes; neither fires once the image is there and
    /// whole. The receive endpoint's note says what it is and the capped receive buffer
    /// names the receive endpoint's cost.
    #[test]
    fn a_receive_endpoint_with_no_image_and_an_image_with_a_gap_are_warnings() {
        let dir = TestDir::new("udp-receive");
        let mut counters = CountersFile::create(&dir.0, 128).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let endpoint_id = 11i64;
        let channel = "cavalry:udp?endpoint=0.0.0.0:40123";
        let endpoint_key = endpoint_id.to_le_bytes();
        let allocate = |counters: &mut CountersFile, id, name: &str| {
            counters
                .allocate(id, &endpoint_key, &format!("{name} {channel}"))
                .expect("room")
        };
        let status = allocate(&mut counters, CHANNEL_STATUS, "channel.status");
        status.set(1);
        let sndbuf = allocate(
            &mut counters,
            CHANNEL_SOCKET_SNDBUF,
            "channel.socket_sndbuf",
        );
        sndbuf.set(1024);
        let rcvbuf = allocate(
            &mut counters,
            CHANNEL_SOCKET_RCVBUF,
            "channel.socket_rcvbuf",
        );
        rcvbuf.set(RCVBUF_EXPECTED - 1);
        let publications = allocate(&mut counters, CHANNEL_PUBLICATIONS, "channel.publications");
        publications.set(0);
        let images = allocate(&mut counters, CHANNEL_IMAGES, "channel.images");
        images.set(0);

        // No image yet: the warning names the endpoint; the capped receive buffer names
        // the receive endpoint's cost; the send buffer, which a receive endpoint never
        // uses, raises nothing; the note says receive endpoint.
        let report = examine(&dir.0);
        let found = finding(&report, "udp-subscription-no-image");
        assert_eq!(found.level, Level::Warn);
        assert!(found.message.contains(channel), "{}", found.message);
        assert!(found.message.contains("setup frame"), "{}", found.message);
        assert_eq!(found.doc, "docs/reference/udp.md#the-receiver");
        let buffers: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.check == "udp-socket-buffers")
            .collect();
        assert_eq!(buffers.len(), 1, "{report}");
        assert!(
            buffers[0].message.starts_with("receive endpoint"),
            "{}",
            buffers[0].message
        );
        assert!(
            buffers[0].message.contains("data datagrams dropped"),
            "{}",
            buffers[0].message
        );
        assert!(
            found
                .message
                .contains("image.created 0 and image.ended 0 are driver-wide"),
            "{}",
            found.message
        );
        assert!(
            found.message.contains("still apply to this endpoint"),
            "{}",
            found.message
        );
        let note = finding(&report, "udp-endpoint");
        assert!(
            note.message.starts_with("receive endpoint"),
            "{}",
            note.message
        );
        assert!(note.message.contains("0 image(s)"), "{}", note.message);
        assert!(
            !report.findings.iter().any(|f| f.check == "udp-image-loss"),
            "{report}"
        );

        // A stream that came and ended on a receive endpoint of this driver, this one
        // or another: the two counts are driver-wide, so the finding prints them, says
        // the file does not name the endpoint, and keeps the checks for a stream that
        // never came.
        let created = counters
            .counter(IMAGE_CREATED.record())
            .expect("a static counter");
        let ended = counters
            .counter(IMAGE_ENDED.record())
            .expect("a static counter");
        created.set(1);
        ended.set(1);
        let report = examine(&dir.0);
        let found = finding(&report, "udp-subscription-no-image");
        assert_eq!(found.level, Level::Warn);
        assert!(
            found
                .message
                .contains("image.created 1 and image.ended 1 are driver-wide"),
            "{}",
            found.message
        );
        assert!(
            found
                .message
                .contains("which one the counters file does not say"),
            "{}",
            found.message
        );
        assert!(
            found.message.contains("still apply to this endpoint"),
            "{}",
            found.message
        );
        assert!(found.message.contains("setup frame"), "{}", found.message);

        // An image with a gap: its high-water mark past its rebuild position.
        images.set(1);
        rcvbuf.set(RCVBUF_EXPECTED);
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&42i64.to_le_bytes());
        key[8..12].copy_from_slice(&5i32.to_le_bytes());
        key[12..].copy_from_slice(&1001i32.to_le_bytes());
        let hwm = counters
            .allocate(IMAGE_HWM_POSITION, &key, "image.hwm_position 42 5 1001")
            .expect("room");
        let rebuild = counters
            .allocate(
                IMAGE_REBUILD_POSITION,
                &key,
                "image.rebuild_position 42 5 1001",
            )
            .expect("room");
        hwm.set(8192);
        rebuild.set(4096);
        let report = examine(&dir.0);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "udp-subscription-no-image"),
            "{report}"
        );
        let found = finding(&report, "udp-image-loss");
        assert_eq!(found.level, Level::Warn);
        assert!(
            found
                .message
                .contains("image 42 on stream 1001 (session 5)"),
            "{}",
            found.message
        );
        assert!(
            found.message.contains("4096 bytes past"),
            "{}",
            found.message
        );
        assert_eq!(found.doc, "docs/reference/udp.md#the-receiver");
        assert!(report.ok(), "warnings do not fail: {report}");

        // The gap mended: nothing but the note.
        rebuild.set(8192);
        let report = examine(&dir.0);
        assert!(
            !report.findings.iter().any(|f| {
                f.check == "udp-image-loss"
                    || f.check == "udp-subscription-no-image"
                    || f.check == "udp-socket-buffers"
            }),
            "{report}"
        );
        assert!(
            finding(&report, "udp-endpoint")
                .message
                .contains("1 image(s)")
        );

        // Closing with nothing on it, a send endpoint after its last linger or a
        // receive endpoint after its last image: the note calls it an endpoint, and
        // no warning fires for it.
        status.set(2);
        images.set(0);
        let report = examine(&dir.0);
        let note = finding(&report, "udp-endpoint");
        assert!(note.message.starts_with("endpoint "), "{}", note.message);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "udp-subscription-no-image"),
            "{report}"
        );
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

    /// A recording that was armed and then started, which every follower of a cluster
    /// has, is nothing to report; one still armed is, with how many wait.
    #[test]
    fn an_armed_recording_is_reported_only_while_it_waits() {
        let dir = TestDir::new("armed");
        let counters = CountersFile::create(&dir.0, 256).expect("counters");
        let ring = CommandRing::create(&dir.0, 4096).expect("ring");
        let _consumer = ring.consumer(Duration::from_secs(10)).expect("consumer");
        let bump = |id: CounterId| counters.counter(id.record()).expect("static").increment();
        bump(RECORDING_ARMED);
        bump(RECORDING_ARMED_STARTED);
        let report = examine(&dir.0);
        assert!(
            !report.to_string().contains("recording.armed"),
            "armed and started is no finding: {report}"
        );
        bump(RECORDING_ARMED);
        bump(RECORDING_ARMED);
        bump(RECORDING_ARMED_DROPPED);
        let report = examine(&dir.0);
        let found = finding(&report, "recording.armed");
        assert_eq!(found.level, Level::Warn);
        assert!(
            found.message.starts_with(
                "1 armed recording(s) wait for an image: recording.armed is 3 and 1 started, 0 \
                 failed and 1 were dropped"
            ),
            "{}",
            found.message
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

    /// A Linux routing table as `/proc/net/route` prints it: two default routes of
    /// different metrics over two ports, the link's /30 over the cable, a /24 that is
    /// down, a /16 that rejects, and a /24 over the wired port.
    const ROUTES: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
        enp3s0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
        eno2\t0000000A\t00000000\t0001\t0\t0\t0\tFCFFFFFF\t0\t0\t0\n\
        tun0\t0000000A\t00000000\t0000\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
        *\t0000000A\t00000000\t0201\t0\t0\t0\t0000FFFF\t0\t0\t0\n\
        enp3s0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";

    /// The route with the longest mask that is up and does not reject carries the
    /// destination, the lowest metric among equals; the loopback is `lo`, which the main
    /// table does not list.
    #[test]
    fn a_destination_routes_over_the_longest_mask_that_is_up() {
        assert_eq!(
            route_interface(ROUTES, Ipv4Addr::new(10, 0, 0, 2)).as_deref(),
            Some("eno2"),
            "the /30 over the cable, not the /24 that is down nor the /16 that rejects"
        );
        assert_eq!(
            route_interface(ROUTES, Ipv4Addr::new(10, 0, 9, 9)).as_deref(),
            Some("enp3s0"),
            "past the /30, the /16 rejects and the default route carries it"
        );
        assert_eq!(
            route_interface(ROUTES, Ipv4Addr::new(192, 168, 1, 7)).as_deref(),
            Some("enp3s0")
        );
        assert_eq!(
            route_interface(ROUTES, Ipv4Addr::new(8, 8, 8, 8)).as_deref(),
            Some("enp3s0"),
            "the default route with the lower metric, listed second"
        );
        assert_eq!(
            route_interface(ROUTES, Ipv4Addr::new(127, 0, 0, 1)).as_deref(),
            Some("lo")
        );
        assert_eq!(route_interface("", Ipv4Addr::new(10, 0, 0, 2)), None);
    }

    /// A channel's endpoint and its interface option are read as the labels hold them,
    /// resolved by the driver, so no name is looked up; an IPv6 endpoint, an unspecified
    /// one and a name give nothing, so the check says nothing of them.
    #[test]
    fn an_endpoints_addresses_are_read_from_its_channel() {
        assert_eq!(
            endpoint_addresses("cavalry:udp?endpoint=10.0.0.2:40123"),
            Some((Ipv4Addr::new(10, 0, 0, 2), None))
        );
        assert_eq!(
            endpoint_addresses("cavalry:udp?endpoint=10.0.0.2:40123|interface=10.0.0.1"),
            Some((Ipv4Addr::new(10, 0, 0, 2), Some(Ipv4Addr::new(10, 0, 0, 1))))
        );
        assert_eq!(
            endpoint_addresses("cavalry:udp?endpoint=10.0.0.2:40123|interface=10.0.0.1:40000"),
            Some((Ipv4Addr::new(10, 0, 0, 2), Some(Ipv4Addr::new(10, 0, 0, 1))))
        );
        assert_eq!(
            endpoint_addresses("cavalry:udp?endpoint=10.0.0.2:40123|interface=0.0.0.0"),
            Some((Ipv4Addr::new(10, 0, 0, 2), None)),
            "an unspecified interface address names no interface"
        );
        assert_eq!(endpoint_addresses("cavalry:udp?endpoint=[::1]:40123"), None);
        assert_eq!(
            endpoint_addresses("cavalry:udp?endpoint=0.0.0.0:40123"),
            None
        );
        assert_eq!(endpoint_addresses("cavalry:ipc"), None);
        assert_eq!(
            endpoint_addresses("cavalry:udp?endpoint=grip-link:40123"),
            None,
            "a name is never in a label, and is not looked up"
        );
    }

    /// The MTU check ADR 0039 names (ADR 0044): an active endpoint whose address routes
    /// over an interface with an MTU below the stream's datagram and its headers is a
    /// warning naming the interface, its MTU and the least that carries the stream, the
    /// interface option choosing the interface when the channel names one; an endpoint
    /// over an interface that carries it, one whose interface has no MTU to read, and
    /// one that is closing are not.
    #[test]
    fn an_interface_mtu_below_the_streams_datagram_is_a_warning() {
        let endpoint = |channel: &str, publications: i64, status: i64| EndpointCounters {
            id: 1,
            channel: channel.to_string(),
            status,
            sndbuf: None,
            rcvbuf: None,
            publications: Some(publications),
            images: Some(0),
            destinations: None,
        };
        let narrow = |interface: &str| match interface {
            "eno2" => Some(1280),
            "enp3s0" => Some(1500),
            _ => None,
        };
        let warnings = |endpoints: &[EndpointCounters]| {
            let mut findings = Vec::new();
            interface_mtu_findings(endpoints, ROUTES, &narrow, &mut findings);
            findings
        };
        let found = warnings(&[
            endpoint("cavalry:udp?endpoint=10.0.0.2:40183", 1, 1),
            endpoint("cavalry:udp?endpoint=192.168.1.7:40182", 0, 1),
            endpoint("cavalry:udp?endpoint=127.0.0.1:40123", 1, 1),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
        let found = &found[0];
        assert_eq!(found.check, "udp-interface-mtu");
        assert_eq!(found.level, Level::Warn);
        assert_eq!(found.doc, DOC_UDP_VALUES);
        assert!(
            found
                .message
                .starts_with("send endpoint cavalry:udp?endpoint=10.0.0.2:40183: its datagrams leave through eno2,"),
            "{}",
            found.message
        );
        for word in [
            "1280 bytes",
            "below the 1436",
            "1408 bytes",
            "raise the interface's MTU to 1436",
        ] {
            assert!(found.message.contains(word), "{word} in {}", found.message);
        }
        // A receive endpoint on the narrow interface, with the direction of its datagrams.
        let found = warnings(&[endpoint("cavalry:udp?endpoint=10.0.0.1:40182", 0, 1)]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].message.starts_with("receive endpoint cavalry:udp?endpoint=10.0.0.1:40182: its datagrams arrive through eno2,"),
            "{}",
            found[0].message
        );
        // The interface option chooses the interface: a destination the table sends over
        // the wide port, bound to the narrow one, and the converse.
        let found = warnings(&[endpoint(
            "cavalry:udp?endpoint=192.168.1.7:40183|interface=10.0.0.1",
            1,
            1,
        )]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].message.contains("leave through eno2,"),
            "{}",
            found[0].message
        );
        let found = warnings(&[endpoint(
            "cavalry:udp?endpoint=10.0.0.2:40183|interface=192.168.1.5",
            1,
            1,
        )]);
        assert!(found.is_empty(), "{found:?}");
        // A closing endpoint is left alone, and so is every endpoint over a wide interface.
        let found = warnings(&[endpoint("cavalry:udp?endpoint=10.0.0.2:40183", 0, 2)]);
        assert!(found.is_empty(), "{found:?}");
        let wide = |_: &str| Some(1500);
        let mut findings = Vec::new();
        interface_mtu_findings(
            &[endpoint("cavalry:udp?endpoint=10.0.0.2:40183", 1, 1)],
            ROUTES,
            &wide,
            &mut findings,
        );
        assert!(findings.is_empty(), "{findings:?}");
        // The check runs on Linux alone and counts as run there, table or no table.
        let mut findings = Vec::new();
        let counted = interface_mtu_checks(&[], &mut findings);
        assert_eq!(counted, usize::from(cfg!(target_os = "linux")));
        assert!(findings.is_empty());
    }

    #[test]
    fn the_cluster_keys_are_checked_together_and_against_the_members_string() {
        let checked = |members: Option<&str>, id: Option<i32>| {
            let mut findings = Vec::new();
            let checks = cluster_key_checks(members, id, &mut findings);
            (checks, findings)
        };
        let members = "0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004\
                       |1,localhost:9010,localhost:9011,localhost:9012,localhost:9013,localhost:9014";

        // No cluster: nothing to check, nothing found.
        assert_eq!(checked(None, None).0, 0);
        assert!(checked(None, None).1.is_empty());
        // A cluster as it should be set.
        let (checks, findings) = checked(Some(members), Some(1));
        assert_eq!(checks, 2);
        assert!(findings.is_empty(), "{findings:?}");

        // One key without the other, each named by the key that is missing; a member id
        // of -1, its default, is no member, as `Config::from_vars` reads it.
        let (_, findings) = checked(Some(members), None);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "cluster-member-id");
        assert_eq!(findings[0].level, Level::Fail);
        assert!(
            findings[0]
                .message
                .contains("CAVALRY_CLUSTER_MEMBER_ID is unset or -1, no member"),
            "{}",
            findings[0].message
        );
        assert!(
            findings[0]
                .message
                .contains("set CAVALRY_CLUSTER_MEMBER_ID")
        );
        assert_eq!(findings[0].doc, "docs/reference/config.md#clustermemberid");
        let (_, findings) = checked(None, Some(1));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "cluster-members");
        assert!(findings[0].message.contains("set CAVALRY_CLUSTER_MEMBERS"));
        assert_eq!(findings[0].doc, "docs/reference/config.md#clustermembers");

        // A string the cluster protocol refuses, with its reason.
        let (_, findings) = checked(Some("0,127.0.0.1:9000"), Some(0));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "cluster-members");
        assert!(
            findings[0].message.contains("members-grammar"),
            "{}",
            findings[0].message
        );
        // An id the string does not hold, with the ids it does.
        let (_, findings) = checked(Some(members), Some(2));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, "cluster-member-id");
        assert!(
            findings[0].message.contains("whose ids are 0, 1"),
            "{}",
            findings[0].message
        );
        // An endpoint whose name resolves to nothing, named with its member and its part.
        let unresolved = members.replace("localhost:9012", "no-such-host.invalid:9012");
        let (_, findings) = checked(Some(&unresolved), Some(0));
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].check, "cluster-members");
        assert!(
            findings[0].message.contains(
                "member 1's log endpoint in CAVALRY_CLUSTER_MEMBERS, no-such-host.invalid:9012"
            ),
            "{}",
            findings[0].message
        );
    }

    /// A counters file with one cluster member's counters, each set as given.
    fn member_file(dir: &TestDir, id: i32, values: &[(CounterId, i64)]) -> CountersReader {
        let mut file = CountersFile::create(&dir.0, 1024).expect("the counters file");
        for (type_id, value) in values {
            let counter = file
                .allocate(*type_id, &id.to_le_bytes(), &format!("cluster {id}"))
                .expect("a counter");
            counter.set(*value);
        }
        CountersReader::open(&dir.0).expect("opens")
    }

    fn cluster_findings(test: &str, id: i32, values: &[(CounterId, i64)]) -> (usize, Vec<Finding>) {
        cluster_findings_in(test, id, None, values)
    }

    /// [`cluster_findings`] with the cluster doctor was given a members string for.
    fn cluster_findings_in(
        test: &str,
        id: i32,
        cluster: Option<&Cluster>,
        values: &[(CounterId, i64)],
    ) -> (usize, Vec<Finding>) {
        let dir = TestDir::new(test);
        let reader = member_file(&dir, id, values);
        let mut findings = Vec::new();
        let checks = cluster_checks(&reader, true, cluster, &mut findings);
        (checks, findings)
    }

    /// The appointed leader canvasses as a follower until it leads, so a member with no
    /// leader is told whether it is the one the others wait for, and where to look next.
    #[test]
    fn a_member_with_no_leader_is_told_whether_it_is_the_appointed_leader() {
        let members = "0,127.0.0.1:9000,127.0.0.1:9001,127.0.0.1:9002,127.0.0.1:9003,127.0.0.1:9004\
                       |1,127.0.0.1:9010,127.0.0.1:9011,127.0.0.1:9012,127.0.0.1:9013,127.0.0.1:9014\
                       |2,127.0.0.1:9020,127.0.0.1:9021,127.0.0.1:9022,127.0.0.1:9023,127.0.0.1:9024";
        let cluster = Cluster::of(Some(members)).expect("the members string parses");
        assert_eq!(cluster.ids(), [0, 1, 2]);
        assert_eq!(cluster.appointed_leader(), 0);
        assert_eq!(Cluster::of(Some("0,nonsense")), None);
        assert_eq!(Cluster::of(None), None);
        let canvass = [
            (CLUSTER_ROLE, 0),
            (CLUSTER_ELECTION_STATE, 1),
            (CLUSTER_LEADERSHIP_TERM_ID, -1),
        ];
        let no_leader = |test: &str, id: i32, cluster: Option<&Cluster>| {
            let (_, findings) = cluster_findings_in(test, id, cluster, &canvass);
            let found: Vec<&str> = findings.iter().map(|finding| finding.check).collect();
            assert_eq!(found, ["cluster-member", "cluster-no-leader"]);
            assert_eq!(findings[1].level, Level::Warn);
            findings[1].message.clone()
        };

        // The member with the lowest id: what it waits for, and the other members' hosts.
        let message = no_leader("no-leader-appointed", 0, Some(&cluster));
        assert!(
            message.starts_with(
                "member 0 is the appointed leader, the member with the lowest id in \
                 CAVALRY_CLUSTER_MEMBERS, and leads no term yet: its election is in canvass, \
                 and it leads once every other member's canvass position has reached it, or \
                 once a quorum, 2 of 3, has, itself among them, after the 60 s startup \
                 canvass timeout"
            ),
            "{message}"
        );
        assert!(
            message.contains(
                "run doctor on each other member's host and check that it reaches that endpoint"
            ),
            "{message}"
        );
        assert!(!message.contains("member 0's host"), "{message}");

        // Another member: the appointed leader by its id, and its host.
        let message = no_leader("no-leader-follower", 2, Some(&cluster));
        assert!(
            message.contains(
                "member 2 follows no leader: its election is in canvass. The appointed \
                 leader, member 0, the member with the lowest id in CAVALRY_CLUSTER_MEMBERS, \
                 is not running"
            ),
            "{message}"
        );
        assert!(
            message.ends_with("run doctor on member 0's host"),
            "{message}"
        );

        // No members string, or one this member is not in: both cases, and how to tell.
        for (test, cluster) in [
            ("no-leader-unknown", None),
            ("no-leader-other", Some(&cluster)),
        ] {
            let message = no_leader(test, if cluster.is_some() { 7 } else { 0 }, cluster);
            assert!(message.contains("which doctor was not given"), "{message}");
            assert!(
                message.contains("If it is another member, it is not running"),
                "{message}"
            );
            assert!(
                message.contains(
                    "If it is this member, it leads once every other member's canvass \
                     position has reached it, or once a quorum has"
                ),
                "{message}"
            );
        }
    }

    /// Every code of the spec's table of election states has the table's name, and a
    /// code it does not hold, as from a newer driver, is printed as its number.
    #[test]
    fn every_election_state_of_the_spec_is_named() {
        let spec = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/cluster.md"),
        )
        .expect("spec/cluster.md");
        let table: Vec<(i64, String)> = spec
            .lines()
            .skip_while(|line| !line.starts_with("| Code | State |"))
            .skip(2)
            .take_while(|line| line.starts_with('|'))
            .map(|line| {
                let cells: Vec<&str> = line.split('|').map(str::trim).collect();
                (cells[1].parse().expect("a code"), cells[2].to_string())
            })
            .collect();
        assert_eq!(table.len(), ELECTION_STATES.len(), "{table:?}");
        for (code, name) in &table {
            assert_eq!(&election_state_name(*code), name, "code {code}");
        }
        for code in [-1, 18, 99] {
            assert_eq!(
                election_state_name(code),
                format!("state {code}, one this doctor does not know")
            );
        }

        // The note and the finding name a state this version's members never enter,
        // and a state the table does not hold by its code.
        let (_, findings) = cluster_findings(
            "cluster-catch-up",
            1,
            &[
                (CLUSTER_ROLE, 0),
                (CLUSTER_ELECTION_STATE, 13),
                (CLUSTER_LEADERSHIP_TERM_ID, 2),
            ],
        );
        assert!(
            findings[0]
                .message
                .contains("a follower in term 2, election follower catch-up:"),
            "{}",
            findings[0].message
        );
        let (_, findings) = cluster_findings(
            "cluster-unknown-state",
            1,
            &[(CLUSTER_ROLE, 0), (CLUSTER_ELECTION_STATE, 23)],
        );
        assert!(
            findings[0]
                .message
                .contains("election state 23, one this doctor does not know: append position"),
            "{}",
            findings[0].message
        );
        assert!(
            findings[1]
                .message
                .contains("its election is in state 23, one this doctor does not know."),
            "{}",
            findings[1].message
        );
    }

    /// The member's counts with a doctor sentence of their own are reported by their
    /// name and that sentence when above 0: a peer of another version, whose consensus
    /// messages the member rejects, beside the `cluster-no-leader` it causes; a follower
    /// that lost its leader's heartbeat and its log image, its election closed again.
    #[test]
    fn a_member_count_with_a_doctor_sentence_is_reported_with_it() {
        let (_, findings) = cluster_findings(
            "cluster-rejected",
            1,
            &[
                (CLUSTER_ROLE, 0),
                (CLUSTER_ELECTION_STATE, 1),
                (CLUSTER_LEADERSHIP_TERM_ID, -1),
                (CLUSTER_MESSAGES_REJECTED, 12),
                (CLUSTER_LEADER_HEARTBEAT_TIMEOUTS, 0),
                (CLUSTER_LOG_IMAGES_LOST, 0),
            ],
        );
        let found: Vec<&str> = findings.iter().map(|finding| finding.check).collect();
        assert_eq!(
            found,
            [
                "cluster-member",
                "cluster-no-leader",
                "cluster.messages_rejected"
            ]
        );
        let rejected = &findings[2];
        assert_eq!(rejected.level, Level::Warn);
        assert_eq!(
            rejected.message,
            "member 1's cluster.messages_rejected is 12: A member sends consensus messages \
             this member cannot read: check that every member runs the same version."
        );
        assert_eq!(rejected.doc, DOC_CLUSTER);

        let (_, findings) = cluster_findings(
            "cluster-heartbeat",
            2,
            &[
                (CLUSTER_ROLE, 0),
                (CLUSTER_ELECTION_STATE, 17),
                (CLUSTER_LEADERSHIP_TERM_ID, 0),
                (CLUSTER_LEADER_HEARTBEAT_TIMEOUTS, 1),
                (CLUSTER_LOG_IMAGES_LOST, 2),
            ],
        );
        let found: Vec<&str> = findings.iter().map(|finding| finding.check).collect();
        assert_eq!(
            found,
            [
                "cluster-member",
                "cluster.leader_heartbeat_timeouts",
                "cluster.log_images_lost"
            ]
        );
        assert!(
            findings[1].message.starts_with(
                "member 2's cluster.leader_heartbeat_timeouts is 1: The follower stopped \
                 hearing its leader"
            ),
            "{}",
            findings[1].message
        );
        assert!(
            findings[2].message.starts_with(
                "member 2's cluster.log_images_lost is 2: The follower lost the leader's log \
                 stream"
            ),
            "{}",
            findings[2].message
        );
    }

    #[test]
    fn a_cluster_member_is_explained_from_its_counters() {
        // No member: no check, nothing found. A stopped driver's: the same.
        let dir = TestDir::new("cluster-none");
        let reader = member_file(&dir, 0, &[]);
        let mut findings = Vec::new();
        assert_eq!(cluster_checks(&reader, true, None, &mut findings), 0);
        assert!(findings.is_empty());
        let follower = [
            (CLUSTER_ROLE, 0),
            (CLUSTER_ELECTION_STATE, 17),
            (CLUSTER_LEADERSHIP_TERM_ID, 0),
            (CLUSTER_APPEND_POSITION, 4096),
            (CLUSTER_COMMIT_POSITION, 4096),
        ];
        let dir = TestDir::new("cluster-stopped");
        let reader = member_file(&dir, 1, &follower);
        assert_eq!(cluster_checks(&reader, false, None, &mut findings), 0);
        assert!(findings.is_empty());

        // A follower that follows: the note alone, with who it is and what it knows.
        let (checks, findings) = cluster_findings("cluster-follower", 1, &follower);
        assert_eq!(checks, 9);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].check, "cluster-member");
        assert_eq!(findings[0].level, Level::Note);
        assert!(
            findings[0].message.contains(
                "cluster member 1, a follower in term 0, election closed: append position \
                 4096, commit position 4096"
            ),
            "{}",
            findings[0].message
        );
        assert_eq!(
            findings[0].doc,
            "docs/how-to/run-a-cluster.md#when-it-does-not-work"
        );

        // A member in canvass with no failure behind it: no leader yet.
        let (_, findings) = cluster_findings(
            "cluster-no-leader",
            2,
            &[
                (CLUSTER_ROLE, 0),
                (CLUSTER_ELECTION_STATE, 1),
                (CLUSTER_LEADERSHIP_TERM_ID, -1),
            ],
        );
        let found: Vec<&str> = findings.iter().map(|finding| finding.check).collect();
        assert_eq!(found, ["cluster-member", "cluster-no-leader"]);
        assert!(
            findings[1].message.contains("election is in canvass"),
            "{}",
            findings[1].message
        );

        // A member whose elections fail: left out, with the remedy.
        let (_, findings) = cluster_findings(
            "cluster-late",
            2,
            &[
                (CLUSTER_ROLE, 0),
                (CLUSTER_ELECTION_STATE, 1),
                (CLUSTER_ELECTIONS_FAILED, 40),
                (CLUSTER_JOIN_POSITION_MISMATCHES, 3),
            ],
        );
        let found: Vec<&str> = findings.iter().map(|finding| finding.check).collect();
        assert_eq!(found, ["cluster-member", "cluster-joined-late"]);
        assert!(
            findings[1]
                .message
                .contains("40 of its elections failed, 3 of them")
        );
        assert!(
            findings[1]
                .message
                .contains("every member on a new log directory")
        );

        // An appointed leader that found members ahead: it will not lead, and says why.
        let (_, findings) = cluster_findings(
            "cluster-behind",
            0,
            &[
                (CLUSTER_ROLE, 0),
                (CLUSTER_ELECTION_STATE, 1),
                (CLUSTER_APPOINTED_LEADER_BEHIND, 2),
            ],
        );
        let found: Vec<&str> = findings.iter().map(|finding| finding.check).collect();
        assert_eq!(found, ["cluster-member", "cluster-leader-behind"]);
        assert_eq!(findings[1].level, Level::Fail);
        assert!(
            findings[1]
                .message
                .contains("found 2 member(s) holding a log ahead of its own, so it does not lead:")
        );

        // An appointed leader that leads and found a member ahead in leader ready: it
        // does not close its election, and that is the same failure.
        let (_, findings) = cluster_findings(
            "cluster-behind-ready",
            0,
            &[
                (CLUSTER_ROLE, 2),
                (CLUSTER_ELECTION_STATE, 8),
                (CLUSTER_LEADERSHIP_TERM_ID, 0),
                (CLUSTER_APPOINTED_LEADER_BEHIND, 1),
            ],
        );
        let found: Vec<&str> = findings.iter().map(|finding| finding.check).collect();
        assert_eq!(found, ["cluster-member", "cluster-leader-behind"]);
        assert_eq!(findings[1].level, Level::Fail);
        assert!(
            findings[1].message.contains(
                "found 1 member(s) holding a log ahead of its own, so it leads term 0 and does \
                 not close its election, which stays in leader ready"
            ),
            "{}",
            findings[1].message
        );

        // A leader that lost its quorum, with a commit position behind its own append. A
        // member it once found ahead is in the count, and its election closed since: no
        // failure.
        let (_, findings) = cluster_findings(
            "cluster-leader",
            0,
            &[
                (CLUSTER_ROLE, 2),
                (CLUSTER_ELECTION_STATE, 17),
                (CLUSTER_LEADERSHIP_TERM_ID, 0),
                (CLUSTER_APPEND_POSITION, 9600),
                (CLUSTER_COMMIT_POSITION, 4800),
                (CLUSTER_LEADER_QUORUM_LOSSES, 1),
                (CLUSTER_APPOINTED_LEADER_BEHIND, 1),
            ],
        );
        let found: Vec<&str> = findings.iter().map(|finding| finding.check).collect();
        assert_eq!(
            found,
            [
                "cluster-member",
                "cluster-quorum-lost",
                "cluster-commit-waits"
            ]
        );
        assert!(
            findings[0]
                .message
                .contains("the leader in term 0, election closed")
        );
        assert!(findings[2].message.contains("4800 bytes wait for a quorum"));
    }
}
