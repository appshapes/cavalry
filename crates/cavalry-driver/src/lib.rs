#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod clock;
mod conductor;
mod error;
mod idle;
mod receiver;
mod recorder;
mod replayer;
mod sender;
mod socket;
pub mod spec;
mod watch;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use cavalry_log::dir::LogDir;
use cavalry_shm::counters::generated::{
    DRIVER_PARKS_OVERDUE, LOG_BUFFER_REJECTED_FRAME, RECORDING_SEGMENTS_PURGED,
    RECORDING_WRITE_FAILED,
};
use cavalry_shm::spsc;

pub use error::DriverError;
pub use idle::{IdleStrategy, Idler};

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
    /// The receiver: rebuilds every `cavalry:udp` stream a subscription wants into an
    /// image (ADR 0042); on the data plane and on the message path.
    Receiver,
}

/// What each driver thread runs first, once, before its loop. A caller that owns the
/// host's CPUs pins the conductor to a CPU of its own here and leaves the recorder and
/// the replayer on the housekeeping CPUs, as the bench does under `--pin` (card 136):
/// the threads are spawned with the caller's affinity otherwise, and a conductor that
/// shares a CPU with the recorder keeps that CPU ticking. The hook returns nothing; what
/// it could not do, it reports through what the caller gave it.
pub type ThreadStart = Arc<dyn Fn(DriverThread) + Send + Sync>;

/// A running driver: its directory, its log directory, and its five threads, the
/// conductor, the recorder, the replayer, the sender and the receiver, with a sixth on
/// macOS, the watch, which does none of the driver's work and ends a park of the five
/// that outlived its timeout (ADR 0176). [`Driver::start`] creates the directory's
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
    receiver: Option<JoinHandle<()>>,
    /// The watch, where the driver has one: the thread that ends a park of the other
    /// five that outlived its timeout (ADR 0176).
    watch: Option<JoinHandle<()>>,
    /// The lock on the directory, held from before the directory was read until the
    /// threads have stopped (`spec/client-protocol.md`, "The driver directory"; ADR
    /// 0079).
    dir_lock: Option<conductor::DirLock>,
}

/// The five threads' places in the watch's list.
const CONDUCTOR: usize = 0;
const RECORDER: usize = 1;
const REPLAYER: usize = 2;
const SENDER: usize = 3;
const RECEIVER: usize = 4;

/// Whether a driver has a watch (ADR 0176).
#[derive(Clone, Copy, Debug)]
enum Watching {
    /// No watch: the five threads store nothing of their parks.
    Off,
    /// A watch.
    On,
    /// A watch whose first park lasts an hour, which stands for a park whose timeout
    /// never comes: a test of the conductor's look at the watch.
    #[cfg(test)]
    OnAndHeld,
}

impl Watching {
    /// A watch where a timed wait has been seen to outlive its timer, which is macOS
    /// (ADR 0171), and none elsewhere: a thread and two stores for each park would be
    /// spent for nothing where 381 million waits showed none.
    const fn for_this_system() -> Self {
        if cfg!(target_os = "macos") {
            Watching::On
        } else {
            Watching::Off
        }
    }

    const fn is_on(self) -> bool {
        !matches!(self, Watching::Off)
    }

    #[cfg(test)]
    fn first_park(self) -> Option<std::time::Duration> {
        matches!(self, Watching::OnAndHeld).then_some(idle::tests::NEVER)
    }
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
    /// actions in the `recording.*` counters, and the conductor, recorder, replayer,
    /// sender and receiver threads start (`spec/client-protocol.md`, "The driver
    /// directory"; `spec/durable-log.md`, "The driver"; `spec/udp.md`, "The sender" and
    /// "The receiver"). On macOS a sixth thread starts after them, the watch (ADR
    /// 0176).
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
    /// named the [`DriverThread`] it is on: where a caller pins the conductor, the
    /// sender and the receiver and places the recorder and the replayer
    /// ([`ThreadStart`]).
    pub fn start_with_threads(
        dir: impl AsRef<Path>,
        log_dir: impl AsRef<Path>,
        idle: IdleStrategy,
        on_thread_start: ThreadStart,
    ) -> Result<Self, DriverError> {
        Self::start_watched(
            dir,
            log_dir,
            idle,
            on_thread_start,
            Watching::for_this_system(),
        )
    }

    /// [`Driver::start_with_threads`] with the watch chosen, which a test chooses and
    /// every other caller takes from the system it runs on.
    fn start_watched(
        dir: impl AsRef<Path>,
        log_dir: impl AsRef<Path>,
        idle: IdleStrategy,
        on_thread_start: ThreadStart,
        watching: Watching,
    ) -> Result<Self, DriverError> {
        let dir = dir.as_ref().to_path_buf();
        let log_dir = log_dir.as_ref().to_path_buf();
        let dir_lock = conductor::prepare_dir(&dir)?;
        // What recovery stamps is read by later runs: the epoch clock (ADR 0109).
        let (log, recovery) = LogDir::open(&log_dir, cavalry_shm::clock::epoch_time())?;
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
        let (receiver_requests, receiver_request_consumer) =
            spsc::queue::<receiver::Request>(spec::RECEIVER_QUEUE_CAPACITY);
        let (receiver_reply_producer, receiver_replies) =
            spsc::queue::<receiver::Reply>(spec::RECEIVER_QUEUE_CAPACITY);
        let receiver_link = conductor::ReceiverLink {
            requests: receiver_requests,
            replies: receiver_replies,
        };
        let mut conductor = conductor::Conductor::create(
            &dir,
            idle,
            link,
            replayer_link,
            sender_link,
            receiver_link,
        )?;
        conductor.record_recovery(&recovery)?;
        let replayer = replayer::Replayer::new(
            &log_dir,
            replayer_request_consumer,
            replayer_reply_producer,
            conductor.counter(LOG_BUFFER_REJECTED_FRAME)?,
            conductor.counter(RECORDING_SEGMENTS_PURGED)?,
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
        let receiver_statics = receiver::Statics::new(|id| conductor.counter(id))?;
        let stop = Arc::new(AtomicBool::new(false));
        // The watch's side of each thread, when this driver has a watch: where each of
        // the five says when its park is due, in the order conductor, recorder,
        // replayer, sender, receiver, and the watch as the conductor sees it.
        let dues: Option<[Arc<watch::WakeDue>; 5]> = watching
            .is_on()
            .then(|| std::array::from_fn(|_| Arc::new(watch::WakeDue::new())));
        let due = |thread: usize| dues.as_ref().map(|dues| Arc::clone(&dues[thread]));
        let watched = Arc::new(watch::Watched::default());
        let parks_overdue = conductor.counter(DRIVER_PARKS_OVERDUE)?;
        let conductor_watch = match watching.is_on() {
            true => Some((
                Arc::clone(&watched),
                conductor.counter(DRIVER_PARKS_OVERDUE)?,
            )),
            false => None,
        };
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
        let replayer_due = due(REPLAYER);
        let replayer_handle = std::thread::Builder::new()
            .name("cavalry-replayer".to_string())
            .spawn(move || {
                on_start(DriverThread::Replayer);
                replayer.run(&flag, replayer_due)
            })
            .map_err(io("start the replayer thread", &log_dir))?;
        conductor.set_replayer_thread(replayer_handle.thread().clone());
        let flag = Arc::clone(&stop);
        let on_start = Arc::clone(&on_thread_start);
        let sender_due = due(SENDER);
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
                sender.run(&flag, sender_due)
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
        let receiver_due = due(RECEIVER);
        let receiver_handle = match std::thread::Builder::new()
            .name("cavalry-receiver".to_string())
            .spawn(move || {
                on_start(DriverThread::Receiver);
                // Built on its own thread, as the sender is: the headers its batched
                // receive fills hold raw pointers and cannot be sent to it (ADR 0042).
                let receiver = receiver::Receiver::new(
                    receiver_request_consumer,
                    receiver_reply_producer,
                    receiver_statics,
                    idle,
                );
                receiver.run(&flag, receiver_due)
            }) {
            Ok(handle) => handle,
            Err(source) => {
                stop.store(true, Ordering::Release);
                replayer_handle.thread().unpark();
                sender_handle.thread().unpark();
                let _ = replayer_handle.join();
                let _ = sender_handle.join();
                return Err(io("start the receiver thread", &dir)(source));
            }
        };
        conductor.set_receiver_thread(receiver_handle.thread().clone());
        let flag = Arc::clone(&stop);
        let on_start = Arc::clone(&on_thread_start);
        let conductor_due = due(CONDUCTOR);
        let conductor_handle = match std::thread::Builder::new()
            .name("cavalry-conductor".to_string())
            .spawn(move || {
                on_start(DriverThread::Conductor);
                conductor.run(&flag, conductor_due, conductor_watch)
            }) {
            Ok(handle) => handle,
            Err(source) => {
                stop.store(true, Ordering::Release);
                replayer_handle.thread().unpark();
                sender_handle.thread().unpark();
                receiver_handle.thread().unpark();
                let _ = replayer_handle.join();
                let _ = sender_handle.join();
                let _ = receiver_handle.join();
                return Err(io("start the conductor thread", &dir)(source));
            }
        };
        let flag = Arc::clone(&stop);
        let on_start = on_thread_start;
        let recorder_due = due(RECORDER);
        let recorder_handle = match std::thread::Builder::new()
            .name("cavalry-recorder".to_string())
            .spawn(move || {
                on_start(DriverThread::Recorder);
                recorder.run(&flag, recorder_due)
            }) {
            Ok(handle) => handle,
            Err(source) => {
                stop.store(true, Ordering::Release);
                replayer_handle.thread().unpark();
                sender_handle.thread().unpark();
                receiver_handle.thread().unpark();
                conductor_handle.thread().unpark();
                let _ = conductor_handle.join();
                let _ = replayer_handle.join();
                let _ = sender_handle.join();
                let _ = receiver_handle.join();
                return Err(io("start the recorder thread", &log_dir)(source));
            }
        };
        // The watch last: it needs the five threads, and the conductor learns its
        // thread through `watched` once it has started. No hook runs on it: it is on no
        // message's path, and it parks for a tenth of a second at a time.
        let watch_handle = match dues {
            None => None,
            Some(dues) => {
                let [
                    conductor_due,
                    recorder_due,
                    replayer_due,
                    sender_due,
                    receiver_due,
                ] = dues;
                let watch = watch::Watch {
                    threads: vec![
                        (conductor_handle.thread().clone(), conductor_due),
                        (recorder_handle.thread().clone(), recorder_due),
                        (replayer_handle.thread().clone(), replayer_due),
                        (sender_handle.thread().clone(), sender_due),
                        (receiver_handle.thread().clone(), receiver_due),
                    ],
                    own: Arc::clone(&watched),
                    parks_overdue,
                    #[cfg(test)]
                    first_park: watching.first_park(),
                };
                let flag = Arc::clone(&stop);
                match std::thread::Builder::new()
                    .name("cavalry-watch".to_string())
                    .spawn(move || watch.run(&flag))
                {
                    Ok(handle) => {
                        let _ = watched.thread.set(handle.thread().clone());
                        Some(handle)
                    }
                    Err(source) => {
                        stop.store(true, Ordering::Release);
                        for handle in [
                            &conductor_handle,
                            &recorder_handle,
                            &replayer_handle,
                            &sender_handle,
                            &receiver_handle,
                        ] {
                            handle.thread().unpark();
                        }
                        let _ = conductor_handle.join();
                        let _ = recorder_handle.join();
                        let _ = replayer_handle.join();
                        let _ = sender_handle.join();
                        let _ = receiver_handle.join();
                        return Err(io("start the watch thread", &dir)(source));
                    }
                }
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
            receiver: Some(receiver_handle),
            watch: watch_handle,
            dir_lock: Some(dir_lock),
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
    /// the replayer, woken if it was parked with no replay, dies with its replays, the
    /// sender closes its sockets, so that every receiver times its images out, and the
    /// receiver closes its own, so that every sender disconnects. The directory stays.
    /// [`DriverError::Conductor`], [`DriverError::Recorder`], [`DriverError::Replayer`],
    /// [`DriverError::Sender`] or [`DriverError::Receiver`] when a thread had panicked.
    pub fn close(mut self) -> Result<(), DriverError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.stop.store(true, Ordering::Release);
        // Every thread is unparked, after the flag is stored, and reads the flag as it
        // wakes. A replayer with no replay and a sender or a receiver with nothing to do
        // are parked until a request or this. Each of the five, when its duty cycle found
        // no work, is parked for at most a millisecond by its idle strategy, and this
        // stop does not wait on that timeout: on one Mac a wait of 1 ms in about 40
        // million did not return until its thread was woken, and the join below then
        // waited for ever (ADR 0171).
        for handle in [
            &self.conductor,
            &self.recorder,
            &self.replayer,
            &self.sender,
            &self.receiver,
            &self.watch,
        ]
        .into_iter()
        .flatten()
        {
            handle.thread().unpark();
        }
        // The watch first: it holds the others' threads, and has nothing to flush.
        if let Some(handle) = self.watch.take() {
            let _ = handle.join();
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
        let receiver = match self.receiver.take() {
            Some(handle) => handle.join().map_err(|_| DriverError::Receiver {
                dir: self.dir.clone(),
            }),
            None => Ok(()),
        };
        // The threads have stopped and the heartbeat holds −1: the directory is the
        // next driver's to take.
        self.dir_lock = None;
        conductor
            .and(recorder)
            .and(replayer)
            .and(sender)
            .and(receiver)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A directory of its own for a test of this file.
    fn test_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cavalry-driver-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A hook that has each thread of `held` park for an hour where it would park for
    /// its timeout, which stands for the park whose timeout never comes, and counts
    /// the threads that have started.
    fn holding(held: &'static [DriverThread], started: &Arc<AtomicUsize>) -> ThreadStart {
        let started = Arc::clone(started);
        Arc::new(move |thread| {
            if held.contains(&thread) {
                idle::tests::PARK_FOR_TEST.set(Some(idle::tests::NEVER));
            }
            started.fetch_add(1, Ordering::AcqRel);
        })
    }

    const ALL: &[DriverThread] = &[
        DriverThread::Conductor,
        DriverThread::Recorder,
        DriverThread::Replayer,
        DriverThread::Sender,
        DriverThread::Receiver,
    ];

    /// `close` on a thread of its own, held to 20 s.
    fn close_within_bound(driver: Driver, what: &str) {
        let (closed, close) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            let _ = closed.send(driver.close());
        });
        let result = close.recv_timeout(Duration::from_secs(20)).expect(what);
        result.expect("closes");
        closer.join().expect("the closing thread ended");
    }

    /// `driver.parks_overdue` of the driver on `dir`.
    fn parks_overdue(dir: &Path) -> i64 {
        cavalry_shm::counters::CountersReader::open(dir)
            .expect("the counters file opens")
            .read(DRIVER_PARKS_OVERDUE.record())
            .expect("a static counter")
    }

    /// A stop does not wait on any thread's park to time out (ADR 0171). Every park of
    /// every thread lasts an hour here, which stands for the park whose timeout never
    /// comes: the backoff's of the conductor and the recorder, and the park of a second
    /// of a sender, a receiver and a replayer with nothing to do. The driver has no
    /// watch, so `close` returns in time only if the stop unparks each of the five.
    #[test]
    fn close_ends_every_thread_though_no_park_would_time_out() {
        let dir = test_dir("close-unparks");
        let started = Arc::new(AtomicUsize::new(0));
        let driver = Driver::start_watched(
            &dir,
            dir.join("log"),
            IdleStrategy::Backoff,
            holding(ALL, &started),
            Watching::Off,
        )
        .expect("starts");
        // Each thread has run its hook, and then long enough for each to park.
        while started.load(Ordering::Acquire) < 5 {
            std::thread::yield_now();
        }
        std::thread::park_timeout(Duration::from_millis(200));
        close_within_bound(
            driver,
            "close returned: a thread that stays parked was not unparked by the stop",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The watch ends a park that outlived its timeout, of each of the five threads
    /// (ADR 0176). One driver for each thread, with a watch, in which that thread alone
    /// parks for an hour where it would park for its timeout. No other thread of it is
    /// held, so a count in `driver.parks_overdue` is the watch's unpark of that thread:
    /// 200 ms after a millisecond for the conductor and the recorder, and 200 ms after
    /// a second for a replayer, a sender and a receiver with nothing to do.
    #[test]
    fn the_watch_ends_a_park_that_outlived_its_timeout_for_each_thread() {
        const EACH: [&[DriverThread]; 5] = [
            &[DriverThread::Conductor],
            &[DriverThread::Recorder],
            &[DriverThread::Replayer],
            &[DriverThread::Sender],
            &[DriverThread::Receiver],
        ];
        for held in EACH {
            let dir = test_dir(&format!("watch-{:?}", held[0]));
            let started = Arc::new(AtomicUsize::new(0));
            let driver = Driver::start_watched(
                &dir,
                dir.join("log"),
                IdleStrategy::Backoff,
                holding(held, &started),
                Watching::On,
            )
            .expect("starts");
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while parks_overdue(&dir) < 1 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the watch did not end the park of the {:?}",
                    held[0]
                );
                std::thread::park_timeout(Duration::from_millis(20));
            }
            close_within_bound(driver, "close returned");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The conductor ends the watch's own park when it outlived its timeout (ADR 0176).
    /// The watch's first park lasts an hour here. No other thread is held, and a watch
    /// that is parked counts nothing, so a count in `driver.parks_overdue` is the
    /// conductor's unpark of the watch.
    #[test]
    fn the_conductor_ends_the_watchs_park_when_it_outlived_its_timeout() {
        let dir = test_dir("watch-held");
        let started = Arc::new(AtomicUsize::new(0));
        let driver = Driver::start_watched(
            &dir,
            dir.join("log"),
            IdleStrategy::Backoff,
            holding(&[], &started),
            Watching::OnAndHeld,
        )
        .expect("starts");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while parks_overdue(&dir) < 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "the conductor did not end the watch's park"
            );
            std::thread::park_timeout(Duration::from_millis(20));
        }
        close_within_bound(driver, "close returned");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A driver without a watch counts nothing and starts no sixth thread: where no
    /// timed wait has been seen to outlive its timer, the five threads store nothing
    /// of their parks.
    #[test]
    fn a_driver_without_a_watch_counts_no_overdue_park_though_a_thread_is_held() {
        let dir = test_dir("no-watch");
        let started = Arc::new(AtomicUsize::new(0));
        let driver = Driver::start_watched(
            &dir,
            dir.join("log"),
            IdleStrategy::Backoff,
            holding(&[DriverThread::Recorder], &started),
            Watching::Off,
        )
        .expect("starts");
        assert!(driver.watch.is_none());
        std::thread::park_timeout(Duration::from_millis(400));
        assert_eq!(parks_overdue(&dir), 0);
        close_within_bound(driver, "close returned");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
