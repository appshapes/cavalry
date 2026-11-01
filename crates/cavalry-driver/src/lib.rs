#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod conductor;
mod error;
mod idle;
mod recorder;
mod replayer;
mod sender;
mod socket;
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

/// A thread of the driver, as [`Driver::start_with_threads`] names it to the hook each
/// thread runs first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverThread {
    /// The conductor: the duty cycle, on the data plane.
    Conductor,
    /// The recorder: the durable log's writer, on the data plane but not on the
    /// message path.
    Recorder,
    /// The replayer: the durable log's reader, which plays a recording into a replay's
    /// log buffer (ADR 0030); on the data plane but not on the live message path.
    Replayer,
    /// The sender: puts every `cavalry:udp` publication's log on the wire (ADR 0041);
    /// on the data plane and on the message path.
    Sender,
}

/// What each driver thread runs first, once, before its loop. A caller that owns the
/// host's CPUs pins the conductor to a CPU of its own here and leaves the recorder and
/// the replayer on the housekeeping CPUs, as the bench does under `--pin` (card 136):
/// the threads are spawned with the caller's affinity otherwise, and a conductor that
/// shares a CPU with the recorder keeps that CPU ticking. The hook returns nothing; what
/// it could not do, it reports through what the caller gave it.
pub type ThreadStart = Arc<dyn Fn(DriverThread) + Send + Sync>;

/// A running driver: its directory, its log directory, and its four threads, the
/// conductor, the recorder, the replayer and the sender. [`Driver::start`] creates the directory's
/// files, opens the log directory under its lock and recovers it, and starts the
/// threads; [`Driver::close`] stops them, storing −1 in the command ring's heartbeat so
/// that every client learns `driver-closed`, stopping every live recording at what was
/// written, and leaves the directory for `doctor` (`spec/client-protocol.md`, "The
/// driver directory"; ADR 0025, ADR 0030). Dropping a driver closes it the same way.
pub struct Driver {
    dir: PathBuf,
    log_dir: PathBuf,
    stop: Arc<AtomicBool>,
    conductor: Option<JoinHandle<()>>,
    recorder: Option<JoinHandle<()>>,
    replayer: Option<JoinHandle<()>>,
    sender: Option<JoinHandle<()>>,
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
    /// actions in the `recording.*` counters, and the conductor, recorder, replayer and
    /// sender threads start (`spec/client-protocol.md`, "The driver directory";
    /// `spec/durable-log.md`, "The driver"; `spec/udp.md`, "The sender").
    pub fn start(dir: impl AsRef<Path>, log_dir: impl AsRef<Path>) -> Result<Self, DriverError> {
        Self::start_with(dir, log_dir, IdleStrategy::Backoff)
    }

    /// [`Driver::start`] with the idle strategy of every thread chosen: what each does
    /// on a duty cycle that found no work.
    pub fn start_with(
        dir: impl AsRef<Path>,
        log_dir: impl AsRef<Path>,
        idle: IdleStrategy,
    ) -> Result<Self, DriverError> {
        Self::start_with_threads(dir, log_dir, idle, Arc::new(|_| {}))
    }

    /// [`Driver::start_with`] with a hook each thread runs first, once, before its loop,
    /// named the [`DriverThread`] it is on: where a caller pins the conductor and the
    /// sender and places the recorder and the replayer ([`ThreadStart`]).
    pub fn start_with_threads(
        dir: impl AsRef<Path>,
        log_dir: impl AsRef<Path>,
        idle: IdleStrategy,
        on_thread_start: ThreadStart,
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
        let (replayer_requests, replayer_request_consumer) =
            spsc::queue::<replayer::Request>(spec::REPLAYER_QUEUE_CAPACITY);
        let (replayer_reply_producer, replayer_replies) =
            spsc::queue::<replayer::Reply>(spec::REPLAYER_QUEUE_CAPACITY);
        let replayer_link = conductor::ReplayerLink {
            requests: replayer_requests,
            replies: replayer_replies,
        };
        let (sender_requests, sender_request_consumer) =
            spsc::queue::<sender::Request>(spec::SENDER_QUEUE_CAPACITY);
        let (sender_reply_producer, sender_replies) =
            spsc::queue::<sender::Reply>(spec::SENDER_QUEUE_CAPACITY);
        let sender_link = conductor::SenderLink {
            requests: sender_requests,
            replies: sender_replies,
        };
        let mut conductor =
            conductor::Conductor::create(&dir, idle, link, replayer_link, sender_link)?;
        conductor.record_recovery(&recovery)?;
        let replayer = replayer::Replayer::new(
            &log_dir,
            replayer_request_consumer,
            replayer_reply_producer,
            conductor.counter(LOG_BUFFER_REJECTED_FRAME)?,
            idle,
        )
        .map_err(|error| DriverError::LogDir(error.into()))?;
        let recorder = recorder::Recorder::new(
            log,
            request_consumer,
            reply_producer,
            conductor.counter(RECORDING_WRITE_FAILED)?,
            conductor.counter(LOG_BUFFER_REJECTED_FRAME)?,
            idle,
        );
        // The sender's counters here, where the conductor is; the sender itself is built
        // on its thread below, since the headers it preallocates are not `Send`.
        let sender_statics = sender::Statics::new(|id| conductor.counter(id))?;
        let stop = Arc::new(AtomicBool::new(false));
        let io = |what: &'static str, path: &PathBuf| {
            let path = path.clone();
            move |source| DriverError::Io { what, path, source }
        };
        // The replayer first: it parks while it holds no replay, and the conductor
        // unparks it with every request, so the conductor holds its thread before its
        // own loop starts (`conductor::Conductor::set_replayer_thread`); the sender the
        // same way, since it parks while it holds nothing to send (ADR 0041).
        let flag = Arc::clone(&stop);
        let on_start = Arc::clone(&on_thread_start);
        let replayer_handle = std::thread::Builder::new()
            .name("cavalry-replayer".to_string())
            .spawn(move || {
                on_start(DriverThread::Replayer);
                replayer.run(&flag)
            })
            .map_err(io("start the replayer thread", &log_dir))?;
        conductor.set_replayer_thread(replayer_handle.thread().clone());
        let flag = Arc::clone(&stop);
        let on_start = Arc::clone(&on_thread_start);
        let sender_handle = match std::thread::Builder::new()
            .name("cavalry-sender".to_string())
            .spawn(move || {
                on_start(DriverThread::Sender);
                // Built on its own thread: the headers `sendmmsg` and `recvmmsg` fill
                // hold raw pointers and cannot be sent to it (ADR 0041, acceptance).
                let sender = sender::Sender::new(
                    sender_request_consumer,
                    sender_reply_producer,
                    sender_statics,
                    idle,
                );
                sender.run(&flag)
            }) {
            Ok(handle) => handle,
            Err(source) => {
                stop.store(true, Ordering::Release);
                replayer_handle.thread().unpark();
                let _ = replayer_handle.join();
                return Err(io("start the sender thread", &dir)(source));
            }
        };
        conductor.set_sender_thread(sender_handle.thread().clone());
        let flag = Arc::clone(&stop);
        let on_start = Arc::clone(&on_thread_start);
        let conductor_handle = match std::thread::Builder::new()
            .name("cavalry-conductor".to_string())
            .spawn(move || {
                on_start(DriverThread::Conductor);
                conductor.run(&flag)
            }) {
            Ok(handle) => handle,
            Err(source) => {
                stop.store(true, Ordering::Release);
                replayer_handle.thread().unpark();
                sender_handle.thread().unpark();
                let _ = replayer_handle.join();
                let _ = sender_handle.join();
                return Err(io("start the conductor thread", &dir)(source));
            }
        };
        let flag = Arc::clone(&stop);
        let on_start = on_thread_start;
        let recorder_handle = match std::thread::Builder::new()
            .name("cavalry-recorder".to_string())
            .spawn(move || {
                on_start(DriverThread::Recorder);
                recorder.run(&flag)
            }) {
            Ok(handle) => handle,
            Err(source) => {
                stop.store(true, Ordering::Release);
                replayer_handle.thread().unpark();
                sender_handle.thread().unpark();
                let _ = conductor_handle.join();
                let _ = replayer_handle.join();
                let _ = sender_handle.join();
                return Err(io("start the recorder thread", &log_dir)(source));
            }
        };
        Ok(Self {
            dir,
            log_dir,
            stop,
            conductor: Some(conductor_handle),
            recorder: Some(recorder_handle),
            replayer: Some(replayer_handle),
            sender: Some(sender_handle),
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

    /// Stops the threads and waits for them: the conductor stores −1 in the heartbeat
    /// on its way out, the recorder stops every live recording at what it has written,
    /// the replayer, woken if it was parked with no replay, dies with its replays, and
    /// the sender closes its sockets, so that every receiver times its images out.
    /// The directory stays. [`DriverError::Conductor`], [`DriverError::Recorder`] or
    /// [`DriverError::Replayer`] when a thread had panicked.
    pub fn close(mut self) -> Result<(), DriverError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.stop.store(true, Ordering::Release);
        // A replayer with no replay, and a sender with nothing to send, are parked until
        // a request or this; each reads the flag as it wakes.
        if let Some(handle) = &self.replayer {
            handle.thread().unpark();
        }
        if let Some(handle) = &self.sender {
            handle.thread().unpark();
        }
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
        let replayer = match self.replayer.take() {
            Some(handle) => handle.join().map_err(|_| DriverError::Replayer {
                log_dir: self.log_dir.clone(),
            }),
            None => Ok(()),
        };
        let sender = match self.sender.take() {
            Some(handle) => handle.join().map_err(|_| DriverError::Sender {
                dir: self.dir.clone(),
            }),
            None => Ok(()),
        };
        conductor.and(recorder).and(replayer).and(sender)
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
