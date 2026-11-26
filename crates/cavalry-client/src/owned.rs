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

use std::fs::{File, TryLockError};
use std::io;
use std::sync::Arc;

use cavalry_shm::client_lock::ClientsDir;
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
/// The file is made through a handle on `clients/` that the client opened before it
/// mapped any of the driver's files, and not by its path: so it is in the directory
/// of the driver whose ring gave the client its id, whatever driver has started in
/// that place since ([`cavalry_shm::client_lock`]).
///
/// The client does not remove the file when it lets go: the driver does, once it has
/// taken the lock.
#[derive(Debug)]
pub(crate) struct ClientLock {
    /// The open file the lock is on. Nothing reads it: it is held.
    _file: File,
}

impl ClientLock {
    /// Creates the lock file of client `client_id` through `clients` and takes its
    /// lock.
    ///
    /// An error is a client that has no lock, and a client without its lock sends no
    /// command. `NotFound` is a directory that was removed: a driver has started in
    /// its place, and the caller attaches again from the beginning, with a new id.
    /// `AlreadyExists` is a name that some other hand has taken: the caller takes its
    /// next id. A lock that the system refuses on the file this call has just made is
    /// an error like any other, and the file is removed again.
    pub(crate) fn take(clients: &ClientsDir, client_id: i64) -> io::Result<Self> {
        let file = clients.create(client_id)?;
        if let Err(refused) = file.try_lock() {
            let _ = clients.remove(client_id);
            return Err(match refused {
                TryLockError::WouldBlock => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "the lock file this client has just made is locked by another process",
                ),
                TryLockError::Error(error) => error,
            });
        }
        Ok(Self { _file: file })
    }

    /// Gives the lock up for a client that sent no command, so that the driver never
    /// learned of it and will never remove its file: the file is removed here, through
    /// the handle it was made through, and the lock goes with this value.
    pub(crate) fn abandon(self, clients: &ClientsDir, client_id: i64) {
        let _ = clients.remove(client_id);
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cavalry_proto::client_protocol::{CLIENT_LOCK_DIRECTORY, CLIENT_LOCK_SUFFIX};

    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cavalry-owned-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(CLIENT_LOCK_DIRECTORY)).expect("a directory");
        dir
    }

    #[test]
    fn a_clients_lock_is_held_on_its_file_until_it_is_dropped() {
        let dir = scratch("held");
        let clients = ClientsDir::open(&dir).expect("the handle");
        let lock = ClientLock::take(&clients, 7).expect("the lock");
        let path = cavalry_shm::client_lock::path(&dir, 7, CLIENT_LOCK_SUFFIX);
        // As the driver tries it: a second open of the file, and a try that does not wait.
        let tried = File::open(&path).expect("the file by its name");
        assert!(matches!(tried.try_lock(), Err(TryLockError::WouldBlock)));
        // The name is taken: a second client of that id is refused, and nothing of the
        // first one's is touched.
        let second = ClientLock::take(&clients, 7).expect_err("a second lock of one id");
        assert_eq!(second.kind(), io::ErrorKind::AlreadyExists);
        assert!(matches!(tried.try_lock(), Err(TryLockError::WouldBlock)));
        drop(lock);
        assert!(path.is_file(), "the client does not remove its file");
        tried
            .try_lock()
            .expect("the lock is free once the client let go");
        drop(tried);
        // A client that sent no command removes the file it made, through its handle.
        let unsent = ClientLock::take(&clients, 8).expect("another client's lock");
        let its_path = cavalry_shm::client_lock::path(&dir, 8, CLIENT_LOCK_SUFFIX);
        assert!(its_path.is_file());
        unsent.abandon(&clients, 8);
        assert!(!its_path.exists());
        assert!(path.is_file(), "and no other client's");
        std::fs::remove_dir_all(&dir).expect("remove the directory");
    }

    /// A client of the driver before a restart, stopped between its id and its lock
    /// file: continued, it makes no file in the directory of the driver that started,
    /// under the id it took or any other, and is told that the directory is gone, so
    /// that it attaches again from the beginning.
    #[test]
    fn a_lock_is_not_taken_in_the_directory_of_a_driver_that_started_since() {
        let dir = scratch("restarted");
        let clients = ClientsDir::open(&dir).expect("the earlier client's handle");
        // The next driver starts: what the last one left goes, and its own is made.
        std::fs::remove_dir_all(dir.join(CLIENT_LOCK_DIRECTORY)).expect("removed");
        std::fs::create_dir(dir.join(CLIENT_LOCK_DIRECTORY)).expect("made again");
        let refused = ClientLock::take(&clients, 7).expect_err("no lock there");
        assert_eq!(refused.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            std::fs::read_dir(dir.join(CLIENT_LOCK_DIRECTORY))
                .expect("the new directory")
                .count(),
            0,
            "the next driver's directory holds no file of the earlier client"
        );
        std::fs::remove_dir_all(&dir).expect("remove the directory");
    }
}
