//! A client's one handle on a counter's record that it writes: an image's position or
//! a counter the client added (`spec/client-protocol.md`, "The records a client
//! writes"; ADR 0179).
//!
//! A thread of a client may store into such a record later than any time the driver
//! could wait: a process that was stopped stores when it is continued, and a thread
//! learns that its record went from an event another thread reads. So the driver gives
//! a record to another owner only when its writer has said that it will not store
//! again. That is the release word, and this type is where it is stored: in
//! [`Owned::release`], and in the handle's drop whatever dropped it. The handle is not
//! `Clone`, so one owner holds it at a time, and after its release or its drop nothing
//! stores through it.

use std::fs::File;
use std::path::{Path, PathBuf};

use cavalry_proto::client_protocol::{CLIENT_LOCK_DIRECTORY, CLIENT_LOCK_SUFFIX};
use cavalry_shm::counters::Counter;

/// A client's lock: an exclusive advisory lock on `clients/<client id>.lock` in the
/// driver's directory, taken before the client's first command and held for as long
/// as anything of the client that can store into a counter exists. Every such thing
/// shares the client's state, which owns this, so the lock is let go when the last of
/// them is dropped, and by the system when the process ends. The driver gives a
/// closed or timed-out client's records to another owner when it holds this lock
/// (`spec/client-protocol.md`, "The records a client writes"; ADR 0179).
pub(crate) struct ClientLock {
    _file: File,
    path: PathBuf,
}

impl ClientLock {
    /// Creates the client's lock file and takes its lock, or returns `None` when the
    /// directory has no `clients/`, the file cannot be created or the lock is refused:
    /// the client attaches all the same, and the driver, which finds no lock held at
    /// the client's first command, gives nothing of the client's that carries no
    /// release word to another owner before it starts again.
    pub(crate) fn take(dir: &Path, client_id: i64) -> Option<Self> {
        let path = dir
            .join(CLIENT_LOCK_DIRECTORY)
            .join(format!("{client_id}{CLIENT_LOCK_SUFFIX}"));
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .ok()?;
        if file.try_lock().is_err() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        Some(Self { _file: file, path })
    }
}

impl Drop for ClientLock {
    fn drop(&mut self) {
        // The file goes first and the lock with the handle after it: nothing of the
        // client is left that could store, so a driver that finds no file is told the
        // same as one that takes the lock.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One record this client writes, and the only handle on it in this process.
pub(crate) struct Owned {
    cell: Counter,
}

impl Owned {
    /// Takes the handle the counters file gave for a record the driver allocated for
    /// this client. The caller keeps no clone of `cell`.
    pub(crate) fn new(cell: Counter) -> Self {
        Self { cell }
    }

    /// The record's id.
    pub(crate) fn id(&self) -> u32 {
        self.cell.id()
    }

    /// Stores a position with a release store.
    pub(crate) fn store_release(&self, value: i64) {
        self.cell.store_release(value);
    }

    /// Stores `value`, relaxed.
    pub(crate) fn set(&self, value: i64) {
        self.cell.set(value);
    }

    /// Adds one, relaxed.
    pub(crate) fn increment(&self) {
        self.cell.increment();
    }

    /// Adds `n`, relaxed.
    pub(crate) fn add(&self, n: i64) {
        self.cell.add(n);
    }

    /// Loads a position with an acquire load.
    pub(crate) fn load_acquire(&self) -> i64 {
        self.cell.load_acquire()
    }

    /// Stores the release word: the owner's last store to the record, on the owner's
    /// thread, at a point where it gives the record up. The owner stores nothing
    /// through this handle afterwards. A second release changes nothing.
    pub(crate) fn release(&self) {
        self.cell.release();
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        self.cell.release();
    }
}

impl std::fmt::Debug for Owned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Owned").field("id", &self.id()).finish()
    }
}
