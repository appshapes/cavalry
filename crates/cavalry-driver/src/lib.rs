#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod conductor;
mod error;
mod idle;
mod recorder;
pub mod spec;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use cavalry_log::dir::LogDir;
use cavalry_shm::counters::generated::{LOG_BUFFER_REJECTED_FRAME, RECORDING_WRITE_FAILED};
use cavalry_shm::spsc;

pub use error::DriverError;
pub use idle::IdleStrategy;

/// A running driver: its directory, its log directory, and its two threads, the
/// conductor and the recorder. [`Driver::start`] creates the directory's files, opens
/// the log directory under its lock and recovers it, and starts both threads;
/// [`Driver::close`] stops them, storing −1 in the command ring's heartbeat so that
/// every client learns `driver-closed`, stopping every live recording at what was
/// written, and leaves the directory for `doctor` (`spec/client-protocol.md`, "The
/// driver directory"; ADR 0025). Dropping a driver closes it the same way.
pub struct Driver {
    dir: PathBuf,
    log_dir: PathBuf,
    stop: Arc<AtomicBool>,
    conductor: Option<JoinHandle<()>>,
    recorder: Option<JoinHandle<()>>,
}

impl Driver {
    /// The directory a driver uses when none is given: `/dev/shm/cavalry-<user>` on
    /// Linux when `/dev/shm` exists, and `<tmp>/cavalry-<user>` otherwise
    /// (`spec/client-protocol.md`, "The driver directory").
    pub fn default_dir() -> PathBuf {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "default".to_string());
        let name = format!("cavalry-{user}");
        let shm = PathBuf::from("/dev/shm");
        if cfg!(target_os = "linux") && shm.is_dir() {
            shm.join(name)
        } else {
            std::env::temp_dir().join(name)
        }
    }

    /// The log directory a driver uses when none is given, the rule ADR 0025 fixes:
    /// `$XDG_DATA_HOME/cavalry/log` when that variable is set, else
    /// `$HOME/.local/share/cavalry/log`, and `/var/lib/cavalry/log` when the driver runs
    /// as root (`USER` is `root`, or `HOME` is `/root`); `None` with neither a home nor
    /// root, which a driver refuses to start on. Never under the driver directory, which
    /// is memory. The `log.dir` configuration key, its flag and its variable arrive with
    /// the control-plane feature of card 93.
    pub fn default_log_dir() -> Option<PathBuf> {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let root = var("USER").is_some_and(|user| user == "root")
            || var("HOME").is_some_and(|home| home == "/root");
        if root {
            return Some(PathBuf::from("/var/lib/cavalry/log"));
        }
        if let Some(data) = var("XDG_DATA_HOME") {
            return Some(PathBuf::from(data).join("cavalry").join("log"));
        }
        var("HOME").map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("cavalry")
                .join("log")
        })
    }

    /// Starts a driver on `dir`, recording into `log_dir`, with the default idle strategy,
    /// [`IdleStrategy::Backoff`]. A directory whose driver is alive is
    /// [`DriverError::Active`]; one whose driver is gone is removed and created anew; a
    /// log directory another driver holds is [`DriverError::LogDir`]; the counters file,
    /// the command ring and the broadcast buffer are created with the sizes of [`spec`],
    /// the log directory is opened under its lock and recovered, with the recovery's
    /// actions in the `recording.*` counters, and the conductor and recorder threads
    /// start (`spec/client-protocol.md`, "The driver directory"; `spec/durable-log.md`,
    /// "The driver").
    pub fn start(dir: impl AsRef<Path>, log_dir: impl AsRef<Path>) -> Result<Self, DriverError> {
        Self::start_with(dir, log_dir, IdleStrategy::Backoff)
    }

    /// [`Driver::start`] with the idle strategy of both threads chosen: what each does
    /// on a duty cycle that found no work.
    pub fn start_with(
        dir: impl AsRef<Path>,
        log_dir: impl AsRef<Path>,
        idle: IdleStrategy,
    ) -> Result<Self, DriverError> {
        let dir = dir.as_ref().to_path_buf();
        let log_dir = log_dir.as_ref().to_path_buf();
        conductor::prepare_dir(&dir)?;
        let (log, recovery) = LogDir::open(&log_dir, conductor::now_nanos())?;
        let (requests, request_consumer) =
            spsc::queue::<recorder::Request>(spec::RECORDER_QUEUE_CAPACITY);
        let (reply_producer, replies) =
            spsc::queue::<recorder::Reply>(spec::RECORDER_QUEUE_CAPACITY);
        let link = conductor::RecorderLink {
            requests,
            replies,
            next_recording_id: i64::try_from(log.catalog().slots()).unwrap_or(i64::MAX),
        };
        let conductor = conductor::Conductor::create(&dir, idle, link)?;
        conductor.record_recovery(&recovery)?;
        let recorder = recorder::Recorder::new(
            log,
            request_consumer,
            reply_producer,
            conductor.counter(RECORDING_WRITE_FAILED)?,
            conductor.counter(LOG_BUFFER_REJECTED_FRAME)?,
            idle,
        );
        let stop = Arc::new(AtomicBool::new(false));
        let io = |what: &'static str, path: &PathBuf| {
            let path = path.clone();
            move |source| DriverError::Io { what, path, source }
        };
        let flag = Arc::clone(&stop);
        let conductor_handle = std::thread::Builder::new()
            .name("cavalry-conductor".to_string())
            .spawn(move || conductor.run(&flag))
            .map_err(io("start the conductor thread", &dir))?;
        let flag = Arc::clone(&stop);
        let recorder_handle = match std::thread::Builder::new()
            .name("cavalry-recorder".to_string())
            .spawn(move || recorder.run(&flag))
        {
            Ok(handle) => handle,
            Err(source) => {
                stop.store(true, Ordering::Release);
                let _ = conductor_handle.join();
                return Err(io("start the recorder thread", &log_dir)(source));
            }
        };
        Ok(Self {
            dir,
            log_dir,
            stop,
            conductor: Some(conductor_handle),
            recorder: Some(recorder_handle),
        })
    }

    /// The driver's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The driver's log directory.
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// Stops both threads and waits for them: the conductor stores −1 in the heartbeat
    /// on its way out, and the recorder stops every live recording at what it has
    /// written. The directory stays. [`DriverError::Conductor`] or
    /// [`DriverError::Recorder`] when a thread had panicked.
    pub fn close(mut self) -> Result<(), DriverError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.stop.store(true, Ordering::Release);
        let conductor = match self.conductor.take() {
            Some(handle) => handle.join().map_err(|_| DriverError::Conductor {
                dir: self.dir.clone(),
            }),
            None => Ok(()),
        };
        let recorder = match self.recorder.take() {
            Some(handle) => handle.join().map_err(|_| DriverError::Recorder {
                log_dir: self.log_dir.clone(),
            }),
            None => Ok(()),
        };
        conductor.and(recorder)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Driver")
            .field("dir", &self.dir)
            .field("log_dir", &self.log_dir)
            .field("running", &self.conductor.is_some())
            .finish()
    }
}
