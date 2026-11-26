//! The name of a client's lock file in the driver's directory, and the identity of a
//! file that it is built from (`spec/client-protocol.md`, "The records a client
//! writes"; ADR 0179).
//!
//! The driver gives the records of a client that has gone to other owners when it
//! holds that client's lock, so the file it tries must be the one that client holds
//! and no other process's. A client's id alone does not say that: ids begin again with
//! each driver, and a client makes its file by a path, which names whatever driver
//! holds the directory at that moment. So the name also carries which counters file
//! the client has mapped, by the file's device and file numbers, and the driver looks
//! only at names that carry its own.
//!
//! That cannot name another driver's client, and no clock is in the reason. The system
//! gives two files that exist at once on one device different file numbers. A client
//! has its counters file open when it makes its lock file; a driver has created its own
//! counters file before it creates `clients/`, and keeps it open for as long as it
//! runs. So at the moment a lock file is made in a running driver's `clients/`, both
//! counters files are open, and the two names agree only when the two are one file.

use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use cavalry_proto::client_protocol::CLIENT_LOCK_DIRECTORY;

/// Which file a mapping is of: the device it is on and its file number there, read
/// from the open file that was mapped. Two files that exist at the same time never
/// share both, so two mappings that are held at once are of one file exactly when
/// their identities are equal. A number is used again once its file is removed and no
/// process has it open, so an identity is compared only with one that is held at the
/// same time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId {
    device: u64,
    number: u64,
}

impl FileId {
    /// The identity of the open file `file`.
    pub fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            number: metadata.ino(),
        })
    }

    /// The identity of the file that `path` names now.
    pub fn at(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            number: metadata.ino(),
        })
    }

    /// The path, in driver directory `dir`, of the file of client `client_id` of the
    /// driver whose counters file this is, with `suffix`, `.lock` or `.gone`:
    /// `clients/<client id>-<device>-<file number>` and the suffix, the two numbers
    /// in hexadecimal.
    pub fn client_file(&self, dir: &Path, client_id: i64, suffix: &str) -> PathBuf {
        dir.join(CLIENT_LOCK_DIRECTORY).join(format!(
            "{client_id}-{:x}-{:x}{suffix}",
            self.device, self.number
        ))
    }
}

/// The client id in the name of a file of `clients/` with `suffix`, whichever counters
/// file it names: the decimal number before the first hyphen. `None` for a name that
/// is no such file.
pub fn client_id(file_name: &str, suffix: &str) -> Option<i64> {
    let (id, counters) = file_name.strip_suffix(suffix)?.split_once('-')?;
    let (device, number) = counters.split_once('-')?;
    let hexadecimal = |text: &str| u64::from_str_radix(text, 16).is_ok();
    if !hexadecimal(device) || !hexadecimal(number) || id.starts_with('+') {
        return None;
    }
    id.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_carries_the_client_and_the_counters_files_identity() {
        let id = FileId {
            device: 0x0100_0004,
            number: 0xabcd,
        };
        let path = id.client_file(Path::new("/d"), 41, ".lock");
        assert_eq!(path, Path::new("/d/clients/41-1000004-abcd.lock"));
        assert_eq!(
            id.client_file(Path::new("/d"), 41, ".gone"),
            Path::new("/d/clients/41-1000004-abcd.gone")
        );
        // Another counters file on the same device: no name of the one is a name of
        // the other, for any client id.
        let other = FileId {
            device: 0x0100_0004,
            number: 0xabce,
        };
        assert_ne!(
            other.client_file(Path::new("/d"), 41, ".lock"),
            id.client_file(Path::new("/d"), 41, ".lock")
        );
        assert_eq!(client_id("41-1000004-abcd.lock", ".lock"), Some(41));
        assert_eq!(client_id("41-1000004-abcd.gone", ".gone"), Some(41));
        for name in [
            "41.lock",
            "41-.lock",
            "41-1000004.lock",
            "41-1000004-.lock",
            "41-x-abcd.lock",
            "x-1-2.lock",
            "+41-1-2.lock",
            "41-1000004-abcd.gone",
            "notes",
        ] {
            assert_eq!(client_id(name, ".lock"), None, "{name}");
        }
    }

    #[test]
    fn two_files_that_exist_at_once_have_different_identities_and_one_file_has_one() {
        let dir = std::env::temp_dir().join(format!("cavalry-file-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let first = dir.join("first");
        let second = dir.join("second");
        let held = File::create(&first).expect("a file");
        let other = File::create(&second).expect("another file");
        let id = FileId::of(&held).expect("an open file's identity");
        assert_eq!(FileId::at(&first).expect("the same file by its path"), id);
        assert_ne!(FileId::of(&other).expect("the other's identity"), id);
        // The name is given to another file while the first is still open: the path's
        // identity is the new file's, and the open file keeps its own.
        std::fs::remove_file(&first).expect("remove the name");
        let again = File::create(&first).expect("a new file of that name");
        assert_ne!(FileId::of(&again).expect("the new file's identity"), id);
        assert_ne!(FileId::at(&first).expect("the path's identity"), id);
        assert_eq!(FileId::of(&held).expect("the held file's identity"), id);
        std::fs::remove_dir_all(&dir).expect("remove the directory");
    }
}
