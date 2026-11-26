//! Allocation: a new shared file is given its room before it is mapped, so that a
//! driver directory with no room is the error of the call that creates the file, by
//! name, on every Linux kernel
//! ([ADR 0175](../../../docs/decisions/0175-give-a-new-shared-file-its-room-before-it-is-mapped.md)).
//!
//! A file that is only given its length is sparse: the filesystem finds a page for it
//! when a mapping first touches one, and when it has none the kernel answers the touch
//! with a bus error, which ends the process. The populate advice of
//! [`pretouch`](crate::pretouch) turns that into an error where the kernel has it,
//! Linux 5.14 and later; a kernel without it took the touch loop, and the signal. Asking
//! for the room first needs neither: the filesystem refuses, the creation fails, and
//! the file is removed (ADR 0172).

use std::fs::File;
use std::io;

/// Gives a new, empty file its length and, on Linux, the room for every byte of it.
///
/// On Linux the filesystem is first asked how much room it has free, and a file
/// longer than that is refused with nothing asked of the filesystem: an allocation
/// that cannot be met takes all the room there is before it fails and gives it back,
/// which costs the caller tens of milliseconds and leaves the filesystem full for
/// its other users meanwhile. The answer can be stale by the next call, so it only
/// ever refuses; `fallocate` then asks for the room itself and fails with the
/// filesystem's own error when there is none. Nothing is written and no page is
/// touched. A filesystem that cannot allocate ahead (`EOPNOTSUPP`) is written through
/// with zeros, which asks for the same room a block at a time. Elsewhere the file is
/// given its length alone, as before: ADR 0175 has what was measured on macOS and why.
pub(crate) fn sized(file: &File, length: u64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::sized(file, length)
    }
    #[cfg(not(target_os = "linux"))]
    {
        file.set_len(length)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::File;
    use std::io::{self, Write};

    use nix::errno::Errno;
    use nix::fcntl::{FallocateFlags, fallocate};
    use nix::sys::statvfs::fstatvfs;

    /// How often an interrupted `fallocate` is asked again. A signal that the driver
    /// handles, its stop, interrupts the call once; the bound is there so that no
    /// retry is without one (CLAUDE.md, invariant 3).
    const INTERRUPTED_TRIES: usize = 8;

    /// The zeros a filesystem without `fallocate` is written through with.
    static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];

    pub(super) fn sized(file: &File, length: u64) -> io::Result<()> {
        let wanted = i64::try_from(length).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a file of {length} bytes is longer than a file can be"),
            )
        })?;
        if let Some(free) = free_room(file).filter(|free| *free < length) {
            return Err(no_room(
                io::Error::from(Errno::ENOSPC),
                Some((length, free)),
            ));
        }
        let mut interrupted = 0;
        loop {
            match fallocate(file, FallocateFlags::empty(), 0, wanted) {
                Ok(()) => return Ok(()),
                Err(Errno::EINTR) if interrupted < INTERRUPTED_TRIES => interrupted += 1,
                Err(Errno::EOPNOTSUPP | Errno::ENOSYS) => return written(file, length),
                Err(errno) => return Err(no_room(io::Error::from(errno), None)),
            }
        }
    }

    /// Writes the file through with zeros, for a filesystem that cannot allocate ahead:
    /// each block is asked of the filesystem by a write, which fails with an error and
    /// never with a signal.
    fn written(mut file: &File, length: u64) -> io::Result<()> {
        let mut left = length;
        while left > 0 {
            let now = usize::try_from(left).map_or(ZEROS.len(), |left| left.min(ZEROS.len()));
            file.write_all(&ZEROS[..now])
                .map_err(|error| no_room(error, None))?;
            left -= now as u64;
        }
        Ok(())
    }

    /// The filesystem's free room in bytes, as it says now; `None` where it does not say:
    /// a filesystem that reports no blocks at all, as a tmpfs with no size does, or one
    /// that cannot be asked. Every free block is counted, the ones kept for the
    /// superuser too, so that the answer refuses nothing an allocation could be given.
    fn free_room(file: &File) -> Option<u64> {
        let said = fstatvfs(file).ok()?;
        if said.blocks() == 0 {
            return None;
        }
        // Both are 64 bits wide on every Linux target Cavalry is built for.
        Some(said.blocks_free().saturating_mul(said.fragment_size()))
    }

    /// The error an operator reads when the filesystem did not give a file its room:
    /// what happened, what to do, and the docs anchor. `short` is the file's length and
    /// the filesystem's free room, where the look at the free room refused.
    fn no_room(error: io::Error, short: Option<(u64, u64)>) -> io::Error {
        let short = short.map_or_else(String::new, |(length, free)| {
            format!(": the file needs {length} bytes and the filesystem has {free} free")
        });
        io::Error::new(
            error.kind(),
            format!(
                "could not give the file its room ({error}){short}; the driver \
                 directory's filesystem has no room for it: give it room or move it with \
                 --dir, and run `cavalry doctor` (driver-directory-backing); see \
                 docs/reference/config.md#dir"
            ),
        )
    }

    #[cfg(test)]
    mod tests {
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;

        use super::*;

        fn new_file(name: &str) -> (std::path::PathBuf, File) {
            let path = std::env::temp_dir().join(format!(
                "cavalry-shm-allocate-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            let file = File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("the file is created");
            (path, file)
        }

        #[test]
        fn a_sized_file_has_its_length_and_its_blocks() {
            let (path, file) = new_file("sized");
            let length = 3 * 1024 * 1024 + 4096;
            sized(&file, length).expect("the file is given its room");
            let metadata = file.metadata().expect("metadata");
            assert_eq!(metadata.len(), length);
            // A file that was only given its length holds no block: this one holds them
            // all, counted in units of 512 bytes whatever the filesystem's block is.
            assert!(
                metadata.blocks() * 512 >= length,
                "{} blocks of 512 bytes for {length} bytes",
                metadata.blocks()
            );
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        fn a_file_written_through_has_its_length_in_zeros() {
            let (path, file) = new_file("written");
            let length = 2 * ZEROS.len() as u64 + 4097;
            written(&file, length).expect("the file is written through");
            assert_eq!(file.metadata().expect("metadata").len(), length);
            let mut read = Vec::new();
            File::open(&path)
                .expect("opened")
                .read_to_end(&mut read)
                .expect("read");
            assert_eq!(read.len() as u64, length);
            assert!(read.iter().all(|byte| *byte == 0));
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        fn the_free_room_of_the_tests_own_directory_is_said_or_is_not() {
            // Whatever the answer, it is one a refusal can rest on: a filesystem that
            // reports no blocks says nothing, and no other says less than is free.
            let (path, file) = new_file("free");
            if let Some(free) = free_room(&file) {
                let said = fstatvfs(&file).expect("statvfs");
                assert!(said.blocks() > 0);
                assert!(free >= said.blocks_available().saturating_mul(said.fragment_size()));
            }
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        fn a_file_longer_than_a_file_can_be_is_refused_before_the_filesystem_is_asked() {
            let (path, file) = new_file("too-long");
            let error = sized(&file, u64::MAX).expect_err("refused");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(file.metadata().expect("metadata").len(), 0);
            std::fs::remove_file(path).expect("removed");
        }
    }
}
