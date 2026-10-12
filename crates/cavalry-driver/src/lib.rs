#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod conductor;
mod error;
mod idle;
pub mod spec;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

pub use error::DriverError;
pub use idle::IdleStrategy;

/// A running driver: its directory and its conductor thread. [`Driver::start`] creates
/// the directory's files and starts the conductor; [`Driver::close`] stops it, storing
/// −1 in the command ring's heartbeat so that every client learns `driver-closed`, and
/// leaves the directory for `doctor` (`spec/client-protocol.md`, "The driver directory").
/// Dropping a driver closes it the same way.
pub struct Driver {
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    conductor: Option<JoinHandle<()>>,
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

    /// Starts a driver on `dir` with the default idle strategy, [`IdleStrategy::Backoff`].
    /// A directory whose driver is alive is [`DriverError::Active`]; one whose driver is
    /// gone is removed and created anew; the counters file, the command ring and the
    /// broadcast buffer are created with the sizes of [`spec`], and the conductor thread
    /// starts (`spec/client-protocol.md`, "The driver directory").
    pub fn start(dir: impl AsRef<Path>) -> Result<Self, DriverError> {
        Self::start_with(dir, IdleStrategy::Backoff)
    }

    /// [`Driver::start`] with the conductor's idle strategy chosen: what the thread does
    /// on a duty cycle that found no work.
    pub fn start_with(dir: impl AsRef<Path>, idle: IdleStrategy) -> Result<Self, DriverError> {
        let dir = dir.as_ref().to_path_buf();
        conductor::prepare_dir(&dir)?;
        let conductor = conductor::Conductor::create(&dir, idle)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("cavalry-conductor".to_string())
            .spawn(move || conductor.run(&flag))
            .map_err(|source| DriverError::Io {
                what: "start the conductor thread",
                path: dir.clone(),
                source,
            })?;
        Ok(Self {
            dir,
            stop,
            conductor: Some(handle),
        })
    }

    /// The driver's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stops the conductor and waits for it: it stores −1 in the heartbeat on its way out.
    /// The directory stays. [`DriverError::Conductor`] when the conductor thread had
    /// panicked.
    pub fn close(mut self) -> Result<(), DriverError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.stop.store(true, Ordering::Release);
        match self.conductor.take() {
            Some(handle) => handle.join().map_err(|_| DriverError::Conductor {
                dir: self.dir.clone(),
            }),
            None => Ok(()),
        }
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
            .field("running", &self.conductor.is_some())
            .finish()
    }
}
