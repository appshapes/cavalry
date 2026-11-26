//! A client's one handle on a counter's record that it writes: an image's position or
//! a counter the client added (`spec/client-protocol.md`, "The records a client
//! writes"; ADR 0179).
//!
//! A thread of a client may store into such a record later than any time the driver
//! could wait: a process that was stopped stores when it is continued, and a thread
//! learns that its record went from an event another thread reads. So the driver gives
//! a record to another owner only when its writer has said that it will not store
//! again. That is the release word, and this type is where it is stored: in the
//! handle's drop, and nowhere else. [`Owned::release`] takes the handle and drops it,
//! so that there is one store of the word for one handle by construction: a value is
//! dropped once. A second store would come after the driver may have given the record
//! to another owner, whose word it would then set. The handle is not `Clone`, so one
//! owner holds it at a time, and once it is released there is no handle left to store
//! through. It holds the client's shared state, which owns the client's lock, so no
//! handle outlives the lock, whatever order the things around it are dropped in.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cavalry_proto::client_protocol::{CLIENT_LOCK_DIRECTORY, CLIENT_LOCK_SUFFIX};
use cavalry_shm::counters::Counter;

use crate::conductor::Shared;

/// A client's lock: an exclusive advisory lock on `clients/<client id>.lock` in the
/// driver's directory, taken before the client's first command and held for as long
/// as anything of the client that can store into a counter exists. Every such thing
/// shares the client's state, which owns this, so the lock is let go when the last of
/// them is dropped, and by the system when the process ends. The driver gives a
/// closed or timed-out client's records to another owner when it holds this lock
/// (`spec/client-protocol.md`, "The records a client writes"; ADR 0179).
///
/// The client does not remove the file when it lets go: the driver does, once it has
/// taken the lock. A name in `clients/` is a client id, and ids begin again with each
/// driver, so a client of the driver before a restart that removed "its" file by name
/// could remove the file of a client of the driver that started.
pub(crate) struct ClientLock {
    file: File,
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
        Some(Self { file, path })
    }

    /// Gives the lock up for a client that sent no command, so that the driver never
    /// learned of it and will never remove its file: the file is removed here, when
    /// the name still names this file and no other, by its device and its number.
    pub(crate) fn abandon(self) {
        use std::os::unix::fs::MetadataExt;
        let same = match (self.file.metadata(), std::fs::metadata(&self.path)) {
            (Ok(held), Ok(named)) => held.dev() == named.dev() && held.ino() == named.ino(),
            _ => false,
        };
        if same {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// One record this client writes, and the only handle on it in this process.
pub(crate) struct Owned {
    cell: Counter,
    /// The client's shared state, and with it the client's lock, for as long as this
    /// handle exists.
    _shared: Arc<Shared>,
}

impl Owned {
    /// Takes the handle the counters file gave for a record the driver allocated for
    /// this client. The caller keeps no clone of `cell`.
    pub(crate) fn new(cell: Counter, shared: Arc<Shared>) -> Self {
        Self {
            cell,
            _shared: shared,
        }
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

    /// Gives the record up, on the owner's thread, at a point of the owner's choosing:
    /// the handle is taken and dropped, and its drop stores the release word. Nothing
    /// is left to store through, and nothing is left whose drop could store the word
    /// a second time.
    pub(crate) fn release(self) {
        drop(self);
    }
}

impl Drop for Owned {
    /// The one store of the release word: the owner's last store to the record.
    fn drop(&mut self) {
        self.cell.release();
    }
}

impl std::fmt::Debug for Owned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Owned").field("id", &self.id()).finish()
    }
}
