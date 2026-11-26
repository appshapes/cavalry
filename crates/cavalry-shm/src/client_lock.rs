//! Where a client's lock file is, and how a client makes it
//! (`spec/client-protocol.md`, "The records a client writes"; ADR 0179).
//!
//! The driver gives the records of a client that has gone to other owners when it
//! holds that client's lock, so the file it tries, `clients/<client id>.lock` in its
//! directory, must be the file of its own client of that id and no other process's.
//! A path does not say that. Client ids begin again with each driver, and a path
//! names the directory of whichever driver holds it at that moment: a client that was
//! stopped between taking its id and making its file, while the driver was started
//! again, would make its file in the next driver's `clients/`, under an id that driver
//! gives out again.
//!
//! So a client makes its lock file through a handle on `clients/` itself,
//! [`ClientsDir`], which it opens before it maps any of the driver's files. A driver
//! that starts removes `clients/` with everything else the driver before it left, and
//! only then creates anything of its own; and no file can be made in a directory that
//! has been removed. So when the file is made, the directory the handle is on was
//! still there: the next driver had created nothing yet, and everything the client
//! opened between the handle and the file is of the one driver whose `clients/` that
//! is. No clock, no name and no file number is in that reason.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use cavalry_proto::client_protocol::{CLIENT_LOCK_DIRECTORY, CLIENT_LOCK_SUFFIX};
use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use nix::unistd::{UnlinkatFlags, unlinkat};

/// A handle on `clients/` of a driver's directory: on the directory that had that name
/// when [`ClientsDir::open`] ran, whatever the name is given to afterwards.
#[derive(Debug)]
pub struct ClientsDir {
    dir: File,
}

impl ClientsDir {
    /// Opens `dir/clients`, which must be a directory itself and no link to one: the
    /// system refuses anything else. An error of kind `NotFound` is a directory with
    /// none: a driver that is starting, or one of a version before the lock.
    pub fn open(dir: &Path) -> io::Result<Self> {
        let path = dir.join(CLIENT_LOCK_DIRECTORY);
        let dir = open(
            &path,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self {
            dir: File::from(dir),
        })
    }

    /// Creates the lock file of client `client_id`, `<client id>.lock`, empty, in the
    /// directory this handle is on, and returns it open for writing. It is not locked
    /// yet. The file is created, never opened: `AlreadyExists` when the name is taken.
    /// `NotFound` when the directory has been removed, though another directory may
    /// have its name by now: a driver has started there since the handle was opened.
    pub fn create(&self, client_id: i64) -> io::Result<File> {
        let name = format!("{client_id}{CLIENT_LOCK_SUFFIX}");
        let file = openat(
            &self.dir,
            name.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o666),
        )?;
        Ok(File::from(file))
    }

    /// Removes the lock file of client `client_id` from the directory this handle is
    /// on. For the client that made the file and sent no command, so that the driver
    /// never learned of it: nothing else removes a lock file but the driver. Through
    /// the handle, the name is the one this client made, in the directory it made it
    /// in; by a path it could be the file of another driver's client of the same id.
    pub fn remove(&self, client_id: i64) -> io::Result<()> {
        let name = format!("{client_id}{CLIENT_LOCK_SUFFIX}");
        unlinkat(&self.dir, name.as_str(), UnlinkatFlags::NoRemoveDir)?;
        Ok(())
    }
}

/// The path, in driver directory `dir`, of the file of client `client_id` with
/// `suffix`, `.lock` or `.gone`: `clients/<client id>` and the suffix, the id in
/// decimal. How the driver and `cavalry doctor` name a file, in the directory they
/// read; a client makes its file through [`ClientsDir`] and not by this path.
pub fn path(dir: &Path, client_id: i64, suffix: &str) -> PathBuf {
    dir.join(CLIENT_LOCK_DIRECTORY)
        .join(format!("{client_id}{suffix}"))
}

/// The client id in the name of a file of `clients/` with `suffix`: the decimal
/// number before it, as [`path`] writes it, so that one id has one name. `None` for a
/// name that is no such file, a number with a zero before it among them: `041.lock` is
/// not client 41's file.
pub fn client_id(file_name: &str, suffix: &str) -> Option<i64> {
    let id = file_name.strip_suffix(suffix)?;
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if id.len() > 1 && id.starts_with('0') {
        return None;
    }
    id.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cavalry-client-lock-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(CLIENT_LOCK_DIRECTORY)).expect("a directory");
        dir
    }

    #[test]
    fn a_name_is_the_client_id_and_the_suffix() {
        assert_eq!(
            path(Path::new("/d"), 41, ".lock"),
            Path::new("/d/clients/41.lock")
        );
        assert_eq!(
            path(Path::new("/d"), 41, ".gone"),
            Path::new("/d/clients/41.gone")
        );
        assert_eq!(client_id("41.lock", ".lock"), Some(41));
        assert_eq!(client_id("41.gone", ".gone"), Some(41));
        assert_eq!(client_id("0.lock", ".lock"), Some(0));
        for name in [
            "41.gone",
            ".lock",
            "x.lock",
            "+41.lock",
            "-41.lock",
            "41-1.lock",
            "041.lock",
            "00.lock",
            "notes",
        ] {
            assert_eq!(client_id(name, ".lock"), None, "{name}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)] // makes files in a directory, which Miri's isolation has none of
    fn a_lock_file_is_made_through_the_handle_once() {
        let dir = scratch("once");
        let clients = ClientsDir::open(&dir).expect("the handle");
        let file = clients.create(41).expect("the file is made");
        assert_eq!(file.metadata().expect("its metadata").len(), 0);
        assert!(path(&dir, 41, CLIENT_LOCK_SUFFIX).is_file());
        let again = clients.create(41).expect_err("the name is taken");
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
        clients.remove(41).expect("removed through the handle");
        assert!(!path(&dir, 41, CLIENT_LOCK_SUFFIX).exists());
        assert_eq!(
            clients.remove(41).expect_err("no such name").kind(),
            io::ErrorKind::NotFound
        );
        // A file, and a link to a directory, are no `clients/`.
        let other = dir.join("other");
        std::fs::create_dir(&other).expect("a directory");
        std::fs::write(other.join(CLIENT_LOCK_DIRECTORY), b"").expect("a file of that name");
        assert!(
            ClientsDir::open(&other).is_err(),
            "a file is not a directory"
        );
        let linked = dir.join("linked");
        std::fs::create_dir(&linked).expect("a directory");
        std::os::unix::fs::symlink(
            dir.join(CLIENT_LOCK_DIRECTORY),
            linked.join(CLIENT_LOCK_DIRECTORY),
        )
        .expect("a link");
        assert!(ClientsDir::open(&linked).is_err(), "a link is not followed");
        assert_eq!(
            ClientsDir::open(&dir.join("nowhere"))
                .expect_err("no such directory")
                .kind(),
            io::ErrorKind::NotFound
        );
        std::fs::remove_dir_all(&dir).expect("remove the directory");
    }

    /// What the whole rule stands on: a driver starts again, removes `clients/` and
    /// makes it anew, and a client of the driver before it, which opened its handle
    /// before that, makes no file in the new directory, under any id.
    #[test]
    #[cfg_attr(miri, ignore)] // makes files in a directory, which Miri's isolation has none of
    fn no_file_is_made_through_a_handle_on_a_directory_that_was_removed() {
        let dir = scratch("removed");
        let earlier = ClientsDir::open(&dir).expect("the earlier client's handle");
        earlier
            .create(1)
            .expect("made while the directory is there");
        // The next driver: everything the last one left goes, and then its own.
        std::fs::remove_dir_all(dir.join(CLIENT_LOCK_DIRECTORY)).expect("removed");
        std::fs::create_dir(dir.join(CLIENT_LOCK_DIRECTORY)).expect("made again");
        for id in [1, 2] {
            let refused = earlier
                .create(id)
                .expect_err("a file was made in a directory that is gone");
            assert_eq!(refused.kind(), io::ErrorKind::NotFound, "client {id}");
            assert!(!path(&dir, id, CLIENT_LOCK_SUFFIX).exists());
        }
        // Nor does it remove one there: the next driver's client 1 keeps its file.
        let next_client = ClientsDir::open(&dir).expect("the next client's handle");
        next_client.create(1).expect("the next driver's client 1");
        assert_eq!(
            earlier
                .remove(1)
                .expect_err("removed in a directory that is gone")
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(path(&dir, 1, CLIENT_LOCK_SUFFIX).is_file());
        next_client.remove(1).expect("removed by its own client");
        assert_eq!(
            std::fs::read_dir(dir.join(CLIENT_LOCK_DIRECTORY))
                .expect("the new directory")
                .count(),
            0,
            "the next driver's directory holds nothing of the earlier client"
        );
        // A client of the next driver opens its own handle and makes its file.
        let next = ClientsDir::open(&dir).expect("the next client's handle");
        next.create(1).expect("the next driver's client 1");
        std::fs::remove_dir_all(&dir).expect("remove the directory");
    }
}
