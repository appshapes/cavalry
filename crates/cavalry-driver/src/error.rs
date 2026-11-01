//! Why a driver could not start or stop. Every message says what the operator does
//! next and names the page that explains it.

use std::fmt;
use std::path::PathBuf;

use cavalry_log::dir::DirError;
use cavalry_shm::broadcast::BroadcastError;
use cavalry_shm::command_ring::CommandRingError;
use cavalry_shm::counters::CountersError;
use cavalry_shm::log::LogError;

/// The docs anchor every message names.
const DOC: &str = "docs/reference/client-protocol.md#the-driver-directory";

/// Why [`crate::Driver::start`] or [`crate::Driver::close`] failed.
#[derive(Debug)]
pub enum DriverError {
    /// The directory holds a driver whose heartbeat is fresh.
    Active {
        /// The directory.
        dir: PathBuf,
        /// The creator pid its command ring names.
        pid: i64,
    },
    /// The operating system refused a step on the directory or a thread.
    Io {
        /// The step that failed.
        what: &'static str,
        /// The path it failed on.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
    /// The counters file could not be created.
    Counters(CountersError),
    /// The command ring could not be created.
    CommandRing(CommandRingError),
    /// The broadcast buffer could not be created.
    Broadcast(BroadcastError),
    /// A log buffer could not be created or opened.
    Log(LogError),
    /// The conductor thread panicked; the directory may be half kept.
    Conductor {
        /// The directory.
        dir: PathBuf,
    },
    /// The log directory could not be opened: another driver holds it, or the catalog
    /// or a file in it refused a step of the recovery pass (ADR 0025).
    LogDir(DirError),
    /// The recorder thread panicked; its recordings are live in the catalog, and the
    /// next driver to open the log directory stops them at what was written.
    Recorder {
        /// The log directory.
        log_dir: PathBuf,
    },
    /// The replayer thread panicked; its replays stopped where they were, and nothing
    /// of them is on disk.
    Replayer {
        /// The log directory.
        log_dir: PathBuf,
    },
    /// The sender thread panicked; its publications' receivers see heartbeats stop and
    /// time their images out.
    Sender {
        /// The directory.
        dir: PathBuf,
    },
}

impl DriverError {
    /// The reason's name: `driver-active`, `io`, `counters`, `command-ring`,
    /// `broadcast`, `log`, `conductor`, `log-dir`, `recorder`, `replayer` or `sender`.
    pub const fn reason(&self) -> &'static str {
        match self {
            DriverError::Active { .. } => "driver-active",
            DriverError::Io { .. } => "io",
            DriverError::Counters(_) => "counters",
            DriverError::CommandRing(_) => "command-ring",
            DriverError::Broadcast(_) => "broadcast",
            DriverError::Log(_) => "log",
            DriverError::Conductor { .. } => "conductor",
            DriverError::LogDir(_) => "log-dir",
            DriverError::Recorder { .. } => "recorder",
            DriverError::Replayer { .. } => "replayer",
            DriverError::Sender { .. } => "sender",
        }
    }
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Active { dir, pid } => write!(
                f,
                "driver refused (driver-active): {} holds a driver whose heartbeat is fresh, \
                 created by pid {pid}; stop that driver, or start this one on another directory; \
                 see {DOC}",
                dir.display()
            ),
            DriverError::Io { what, path, source } => write!(
                f,
                "driver failed (io): could not {what} at {}: {source}; check the directory's \
                 permissions and the space on its filesystem; see {DOC}",
                path.display()
            ),
            DriverError::Counters(error) => write!(f, "driver failed (counters): {error}"),
            DriverError::CommandRing(error) => write!(f, "driver failed (command-ring): {error}"),
            DriverError::Broadcast(error) => write!(f, "driver failed (broadcast): {error}"),
            DriverError::Log(error) => write!(f, "driver failed (log): {error}"),
            DriverError::Conductor { dir } => write!(
                f,
                "driver failed (conductor): the conductor thread of the driver at {} panicked; \
                 the directory may be half kept, and the next driver to start removes it; see {DOC}",
                dir.display()
            ),
            DriverError::LogDir(error) => write!(f, "driver failed (log-dir): {error}"),
            DriverError::Recorder { log_dir } => write!(
                f,
                "driver failed (recorder): the recorder thread of the driver on {} panicked; \
                 its recordings stay live in the catalog, and the next driver to open the \
                 directory stops them at what was written; see \
                 docs/reference/durable-log.md#the-driver",
                log_dir.display()
            ),
            DriverError::Replayer { log_dir } => write!(
                f,
                "driver failed (replayer): the replayer thread of the driver on {} panicked; \
                 its replays stopped where they were, and a client that was reading one sees \
                 its images end there; start the replays again on the next driver; see \
                 docs/reference/client-protocol.md#commands",
                log_dir.display()
            ),
            DriverError::Sender { dir } => write!(
                f,
                "driver failed (sender): the sender thread of the driver at {} panicked; its \
                 cavalry:udp publications stopped on the wire, and their receivers time their \
                 images out; add the publications again on the next driver; see \
                 docs/reference/udp.md#the-sender",
                dir.display()
            ),
        }
    }
}

impl std::error::Error for DriverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DriverError::Io { source, .. } => Some(source),
            DriverError::Counters(error) => Some(error),
            DriverError::CommandRing(error) => Some(error),
            DriverError::Broadcast(error) => Some(error),
            DriverError::Log(error) => Some(error),
            DriverError::LogDir(error) => Some(error),
            DriverError::Active { .. }
            | DriverError::Conductor { .. }
            | DriverError::Recorder { .. }
            | DriverError::Replayer { .. }
            | DriverError::Sender { .. } => None,
        }
    }
}

impl From<CountersError> for DriverError {
    fn from(error: CountersError) -> Self {
        DriverError::Counters(error)
    }
}

impl From<CommandRingError> for DriverError {
    fn from(error: CommandRingError) -> Self {
        DriverError::CommandRing(error)
    }
}

impl From<BroadcastError> for DriverError {
    fn from(error: BroadcastError) -> Self {
        DriverError::Broadcast(error)
    }
}

impl From<LogError> for DriverError {
    fn from(error: LogError) -> Self {
        DriverError::Log(error)
    }
}

impl From<DirError> for DriverError {
    fn from(error: DirError) -> Self {
        DriverError::LogDir(error)
    }
}
