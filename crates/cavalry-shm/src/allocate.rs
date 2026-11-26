//! Allocation: a new shared file is given its room before it is mapped, so that a
//! driver directory with no room is the error of the call that creates the file, by
//! name, with the populate advice and without it
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
/// On Linux `fallocate` asks the filesystem for the room, and fails with the
/// filesystem's own error when there is none. Nothing is written and no page is
/// touched. Every signal is held off the calling thread for the length of the call:
/// a tmpfs before Linux 6.11 answers a signal that arrives during it by undoing the
/// whole allocation. A filesystem that cannot allocate ahead (`EOPNOTSUPP`, `ENOSYS`)
/// is written through with zeros, which asks for the same room a block at a time.
///
/// On a tmpfs, which the default driver directory is on wherever `/dev/shm` exists,
/// the filesystem is first asked how much room it has free, and a file longer than
/// that is refused with nothing more asked of it: an allocation that a tmpfs cannot
/// meet takes all the room there is before it fails and gives it back, which costs
/// the caller tens of milliseconds and leaves the filesystem full for its other
/// users meanwhile. The look only ever refuses, and no other filesystem is looked
/// at: XFS and btrfs still count a file that was removed a moment ago, and a look
/// there refuses a file that fits. One tmpfs is looked at that can be wrong the same
/// way, by about 2 MiB a file: one mounted with huge pages (ADR 0175, Consequences).
///
/// A failed allocation may leave the file holding part of its room, as ext4 does: the
/// caller removes the file or leaves it to the driver's next start. Elsewhere than on
/// Linux the file is given its length alone, as before: ADR 0175 has what was measured
/// on macOS and why.
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
    use nix::sys::signal::{SigSet, SigmaskHow};
    use nix::sys::statfs::{TMPFS_MAGIC, fstatfs};

    /// How often an interrupted `fallocate` is asked again. Every signal is held off
    /// the thread for each try. What still interrupts one, on a tmpfs before Linux
    /// 6.11, is a stop or a freeze of the process, a debugger's or a container's
    /// pause, which no mask holds off, and a filesystem may answer so for a reason of
    /// its own. The bound is there so that no retry is without one (CLAUDE.md,
    /// invariant 3).
    const INTERRUPTED_TRIES: usize = 8;

    /// The zeros a filesystem without `fallocate` is written through with.
    static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];

    pub(super) fn sized(file: &File, length: u64) -> io::Result<()> {
        sized_by(file, length, tmpfs_free_room, |file, wanted| {
            fallocate(file, FallocateFlags::empty(), 0, wanted)
        })
    }

    /// [`sized`] with the two questions it asks of the kernel given by the caller, so
    /// that a test can answer them as a kernel or a filesystem that is not at hand
    /// does: `look` is the free room where a look is taken, and `allocate` is the
    /// `fallocate`, which is called with every signal held off the thread.
    fn sized_by(
        file: &File,
        length: u64,
        look: impl FnOnce(&File) -> Option<u64>,
        mut allocate: impl FnMut(&File, i64) -> Result<(), Errno>,
    ) -> io::Result<()> {
        let wanted = i64::try_from(length).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a file of {length} bytes is longer than a file can be"),
            )
        })?;
        if let Some(free) = look(file).filter(|free| *free < length) {
            return Err(refused(
                &io::Error::from(Errno::ENOSPC),
                Some((length, free)),
            ));
        }
        let mut interrupted = 0;
        loop {
            let answer = {
                let _held = SignalsHeld::now();
                allocate(file, wanted)
            };
            match answer {
                Ok(()) => return Ok(()),
                Err(Errno::EINTR) if interrupted < INTERRUPTED_TRIES => interrupted += 1,
                Err(Errno::EOPNOTSUPP | Errno::ENOSYS) => return written(file, length),
                Err(errno) => return Err(refused(&io::Error::from(errno), None)),
            }
        }
    }

    /// Every signal held off the calling thread until this is dropped. `fallocate` on
    /// a tmpfs before Linux 6.11 looks for a pending signal at each page, and answers
    /// one by undoing the whole allocation and failing with `EINTR`: a thread that
    /// takes a signal more often than the call is long, as under a profiler's timer,
    /// would never be given its room. A signal sent meanwhile is delivered when the
    /// mask is put back, after the one call.
    struct SignalsHeld(Option<SigSet>);

    impl SignalsHeld {
        fn now() -> Self {
            // A mask that cannot be set leaves the call as it was without one: the
            // bound on the tries still holds.
            Self(SigSet::all().thread_swap_mask(SigmaskHow::SIG_SETMASK).ok())
        }
    }

    impl Drop for SignalsHeld {
        fn drop(&mut self) {
            if let Some(before) = self.0.take() {
                // Nothing can be done about a mask that cannot be put back, and the
                // call fails only for an argument it is not given here.
                let _ = before.thread_set_mask();
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
                .map_err(|error| refused(&error, None))?;
            left -= now as u64;
        }
        Ok(())
    }

    /// The free room of the file's filesystem in bytes, where that is a tmpfs with a
    /// size; `None` for every other filesystem, for a tmpfs that reports no blocks at
    /// all, as one with no size does, and where the question fails.
    fn tmpfs_free_room(file: &File) -> Option<u64> {
        let said = fstatfs(file).ok()?;
        // The block size's type is the C library's and differs by target: it is
        // widened so that this line is the same on each.
        let block = u64::try_from(i128::from(said.block_size())).ok()?;
        free_room_of(
            said.filesystem_type() == TMPFS_MAGIC,
            said.blocks(),
            said.blocks_free(),
            block,
        )
    }

    /// What [`tmpfs_free_room`] makes of a filesystem's answer. A tmpfs counts its
    /// blocks as they are taken and gives a removed file's back before the removal
    /// returns, so what it says is free is what an allocation can be given; one
    /// mounted with huge pages can say less, by the part of a file's last huge page
    /// that is past the file's end, which an allocation would have taken back (ADR
    /// 0175, Consequences). Every free block is counted: a tmpfs keeps none for the
    /// superuser.
    fn free_room_of(tmpfs: bool, blocks: u64, free: u64, block: u64) -> Option<u64> {
        (tmpfs && blocks > 0).then(|| free.saturating_mul(block))
    }

    /// The error an operator reads when a file was not given its room: what happened,
    /// what to do, and the docs anchor. Only a filesystem's own "no room" (`ENOSPC`)
    /// and "over the quota" (`EDQUOT`) are said to be a want of room. `short` is the
    /// file's length and the filesystem's free room, where the look at the free room
    /// refused.
    fn refused(error: &io::Error, short: Option<(u64, u64)>) -> io::Error {
        let short = short.map_or_else(String::new, |(length, free)| {
            format!(": the file needs {length} bytes and the filesystem has {free} free")
        });
        let errno = error.raw_os_error().map(Errno::from_raw);
        let what = match errno {
            Some(Errno::ENOSPC | Errno::EDQUOT) => {
                "the driver directory's filesystem has no room for it: give it room or \
                 move it with --dir, and run `cavalry doctor` (driver-directory-backing)"
                    .to_string()
            }
            Some(Errno::EINTR) => format!(
                "the filesystem under the driver directory answered that the allocation \
                 was interrupted, {} times running, and whether it has the room was not \
                 found out: ask again, and if it is so again look for what stops or \
                 freezes the driver's process over and over, a debugger or a \
                 container's pause among them",
                INTERRUPTED_TRIES + 1
            ),
            _ => "the filesystem under the driver directory refused the allocation, \
                  and not for want of room: read the error for the reason, a file \
                  size limit of the driver's process and a directory it may not write \
                  among them, or move the directory with --dir"
                .to_string(),
        };
        io::Error::new(
            error.kind(),
            format!(
                "could not give the file its room ({error}){short}; {what}; see \
                 docs/reference/config.md#dir"
            ),
        )
    }

    #[cfg(test)]
    mod tests {
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;
        use std::path::PathBuf;

        use nix::sys::signal::Signal;

        use super::*;

        /// Where `just shm-exhaustion` names a directory on a small tmpfs, 64 MiB in
        /// CI: the tests that need a filesystem with no room run there, and are
        /// ignored everywhere else. The recipe runs them one at a time,
        /// `--test-threads=1`: each counts the room the others take, and all three
        /// fail when they run at once.
        const FULL_DIR: &str = "CAVALRY_SHM_TEST_FULL_DIR";

        fn new_file_in(dir: PathBuf, name: &str) -> (PathBuf, File) {
            std::fs::create_dir_all(&dir).expect("the directory is there");
            let path = dir.join(format!(
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

        fn new_file(name: &str) -> (PathBuf, File) {
            new_file_in(std::env::temp_dir(), name)
        }

        fn full_dir() -> PathBuf {
            PathBuf::from(
                std::env::var(FULL_DIR)
                    .expect("CAVALRY_SHM_TEST_FULL_DIR names a directory on a small tmpfs"),
            )
        }

        /// Fails unless every signal is held off the thread: each test's allocation
        /// calls it, so that an allocation `sized_by` asks without the mask fails
        /// them. Two signals stand for all: a profiler's and the driver's stop.
        fn held() {
            let mask = SigSet::thread_get_mask().expect("the mask is read");
            assert!(mask.contains(Signal::SIGPROF), "SIGPROF is not held off");
            assert!(mask.contains(Signal::SIGTERM), "SIGTERM is not held off");
        }

        /// The `fallocate` itself, as `sized` asks it: what a test's own allocation
        /// asks where it is to be given.
        fn asked(file: &File, wanted: i64) -> Result<(), Errno> {
            held();
            fallocate(file, FallocateFlags::empty(), 0, wanted)
        }

        #[test]
        #[cfg_attr(miri, ignore)]
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
        #[cfg_attr(miri, ignore)]
        fn every_signal_is_held_off_each_allocation_and_the_mask_is_put_back() {
            let before = SigSet::thread_get_mask().expect("the mask is read");
            assert!(!before.contains(Signal::SIGPROF), "the test starts with it");
            let (path, file) = new_file("mask");
            // An allocation that is given, one that fails, and one asked three times:
            // each call is under the mask, and the thread's own is back after each.
            sized_by(&file, 8192, |_| None, asked).expect("given");
            assert_eq!(SigSet::thread_get_mask().expect("the mask is read"), before);
            sized_by(
                &file,
                8192,
                |_| None,
                |_, _| {
                    held();
                    Err(Errno::EIO)
                },
            )
            .expect_err("refused");
            assert_eq!(SigSet::thread_get_mask().expect("the mask is read"), before);
            let mut asks = 0;
            sized_by(
                &file,
                8192,
                |_| None,
                |file, wanted| {
                    assert_ne!(
                        SigSet::thread_get_mask().expect("the mask is read"),
                        before,
                        "ask {asks}"
                    );
                    asks += 1;
                    if asks < 3 {
                        return Err(Errno::EINTR);
                    }
                    asked(file, wanted)
                },
            )
            .expect("given at the third try");
            assert_eq!(SigSet::thread_get_mask().expect("the mask is read"), before);
            sized(&file, 8192).expect("the file is given its room");
            assert_eq!(SigSet::thread_get_mask().expect("the mask is read"), before);
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        #[cfg_attr(miri, ignore)]
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
        #[cfg_attr(miri, ignore)]
        fn a_filesystem_that_cannot_allocate_ahead_is_written_through() {
            for cannot in [Errno::EOPNOTSUPP, Errno::ENOSYS] {
                let (path, file) = new_file("cannot-allocate-ahead");
                let length = ZEROS.len() as u64 + 513;
                let mut asks = 0;
                sized_by(
                    &file,
                    length,
                    |_| None,
                    |_, _| {
                        held();
                        asks += 1;
                        Err(cannot)
                    },
                )
                .expect("the file is written through");
                assert_eq!(asks, 1, "{cannot}");
                let metadata = file.metadata().expect("metadata");
                assert_eq!(metadata.len(), length, "{cannot}");
                assert!(metadata.blocks() * 512 >= length, "{cannot}");
                std::fs::remove_file(path).expect("removed");
            }
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn an_interrupted_allocation_is_asked_again_and_then_given() {
            let (path, file) = new_file("interrupted-twice");
            let mut asks = 0;
            sized_by(
                &file,
                8192,
                |_| None,
                |file, wanted| {
                    held();
                    asks += 1;
                    if asks <= 2 {
                        return Err(Errno::EINTR);
                    }
                    asked(file, wanted)
                },
            )
            .expect("the third ask is given");
            assert_eq!(asks, 3);
            assert_eq!(file.metadata().expect("metadata").len(), 8192);
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn an_allocation_interrupted_at_every_try_ends_at_the_bound_and_says_so() {
            let (path, file) = new_file("interrupted-always");
            let mut asks = 0;
            let error = sized_by(
                &file,
                8192,
                |_| None,
                |_, _| {
                    held();
                    asks += 1;
                    Err(Errno::EINTR)
                },
            )
            .expect_err("refused at the bound");
            assert_eq!(asks, INTERRUPTED_TRIES + 1);
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            let said = error.to_string();
            assert!(said.contains("interrupted, 9 times running"), "{said}");
            assert!(!said.contains("has no room"), "{said}");
            assert!(said.contains("docs/reference/config.md#dir"), "{said}");
            assert_eq!(file.metadata().expect("metadata").len(), 0);
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn only_no_room_and_over_the_quota_are_said_to_be_a_want_of_room() {
            for (errno, no_room) in [
                (Errno::ENOSPC, true),
                (Errno::EDQUOT, true),
                (Errno::EINVAL, false),
                (Errno::EFBIG, false),
                (Errno::EPERM, false),
                (Errno::EIO, false),
            ] {
                let (path, file) = new_file("errno");
                let error = sized_by(
                    &file,
                    8192,
                    |_| None,
                    |_, _| {
                        held();
                        Err(errno)
                    },
                )
                .expect_err("the filesystem's error is the caller's");
                assert_eq!(error.kind(), io::Error::from(errno).kind(), "{errno}");
                let said = error.to_string();
                assert!(
                    said.starts_with("could not give the file its room ("),
                    "{said}"
                );
                assert_eq!(said.contains("has no room for it"), no_room, "{said}");
                assert_eq!(said.contains("not for want of room"), !no_room, "{said}");
                assert!(said.contains("docs/reference/config.md#dir"), "{said}");
                std::fs::remove_file(path).expect("removed");
            }
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn a_look_that_finds_less_than_the_file_refuses_and_nothing_is_allocated() {
            let (path, file) = new_file("look-short");
            let mut asks = 0;
            let error = sized_by(
                &file,
                8192,
                |_| Some(8191),
                |_, _| {
                    asks += 1;
                    Ok(())
                },
            )
            .expect_err("refused by the look");
            assert_eq!(asks, 0);
            assert_eq!(error.kind(), io::Error::from(Errno::ENOSPC).kind());
            let said = error.to_string();
            assert!(
                said.contains("the file needs 8192 bytes and the filesystem has 8191 free"),
                "{said}"
            );
            assert!(said.contains("has no room for it"), "{said}");
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn a_look_that_finds_the_files_length_or_says_nothing_lets_the_allocation_ask() {
            for look in [Some(8192), None] {
                let (path, file) = new_file("look-enough");
                let mut asks = 0;
                sized_by(
                    &file,
                    8192,
                    |_| look,
                    |file, wanted| {
                        asks += 1;
                        asked(file, wanted)
                    },
                )
                .expect("the file is given its room");
                assert_eq!(asks, 1, "{look:?}");
                std::fs::remove_file(path).expect("removed");
            }
        }

        #[test]
        fn only_a_tmpfs_that_reports_its_blocks_says_its_free_room() {
            assert_eq!(free_room_of(true, 16_384, 14_332, 4096), Some(58_703_872));
            assert_eq!(free_room_of(true, 16_384, 0, 4096), Some(0));
            // A tmpfs with no size reports no blocks, and every other filesystem's
            // answer is not taken, whatever it is.
            assert_eq!(free_room_of(true, 0, 0, 4096), None);
            assert_eq!(free_room_of(false, 16_384, 14_332, 4096), None);
            assert_eq!(free_room_of(true, u64::MAX, u64::MAX, 4096), Some(u64::MAX));
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn the_look_is_taken_on_a_tmpfs_with_a_size_and_nowhere_else() {
            // The test's own directory is whatever the host gives it. Whatever that
            // is, the look answers only for a tmpfs that reports its blocks, and then
            // what the filesystem says is free.
            let (path, file) = new_file("free");
            let said = fstatfs(&file).expect("statfs");
            let tmpfs = said.filesystem_type() == TMPFS_MAGIC && said.blocks() > 0;
            match tmpfs_free_room(&file) {
                Some(free) => {
                    assert!(tmpfs);
                    // Another program may take or give room between the two asks: the
                    // answer is a number of whole blocks at least.
                    let block = u64::try_from(i128::from(said.block_size())).expect("a size");
                    assert_eq!(free % block, 0);
                }
                None => assert!(!tmpfs),
            }
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn a_file_longer_than_a_file_can_be_is_refused_before_the_filesystem_is_asked() {
            let (path, file) = new_file("too-long");
            let error = sized(&file, u64::MAX).expect_err("refused");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(file.metadata().expect("metadata").len(), 0);
            std::fs::remove_file(path).expect("removed");
        }

        /// The free room of the directory `just shm-exhaustion` names, which is a
        /// tmpfs with a size: the tests below are nothing without one.
        fn free_in_the_full_dir(file: &File) -> u64 {
            tmpfs_free_room(file).expect("CAVALRY_SHM_TEST_FULL_DIR is on a tmpfs that has a size")
        }

        #[test]
        #[ignore = "needs a directory on a small tmpfs: just shm-exhaustion <dir>"]
        fn in_a_directory_with_no_room_the_allocation_itself_refuses_and_gives_the_room_back() {
            // The look is answered as a filesystem that is not looked at answers it,
            // so that the refusal is the allocation's own: the guarantee, where the
            // look is only the cost.
            let (path, file) = new_file_in(full_dir(), "allocation-refuses");
            let free = free_in_the_full_dir(&file);
            let length = free + 4096;
            let error = sized_by(&file, length, |_| None, asked).expect_err("refused");
            assert_eq!(error.kind(), io::Error::from(Errno::ENOSPC).kind());
            let said = error.to_string();
            assert!(said.contains("has no room for it"), "{said}");
            assert!(!said.contains("the file needs"), "{said}");
            assert_eq!(free_in_the_full_dir(&file), free, "a tmpfs gives it back");
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        #[ignore = "needs a directory on a small tmpfs: just shm-exhaustion <dir>"]
        fn in_a_directory_with_no_room_a_file_written_through_is_refused_by_a_write() {
            let (path, file) = new_file_in(full_dir(), "written-refuses");
            let free = free_in_the_full_dir(&file);
            let length = free + 4096;
            let error = sized_by(&file, length, |_| None, |_, _| Err(Errno::EOPNOTSUPP))
                .expect_err("refused");
            assert_eq!(error.kind(), io::Error::from(Errno::ENOSPC).kind());
            let said = error.to_string();
            assert!(said.contains("has no room for it"), "{said}");
            // What was written stays until the file is removed, which is the caller's
            // to do (ADR 0172): then the room is back.
            drop(file);
            std::fs::remove_file(&path).expect("removed");
            let (path, file) = new_file_in(full_dir(), "written-refuses-after");
            assert_eq!(free_in_the_full_dir(&file), free);
            std::fs::remove_file(path).expect("removed");
        }

        #[test]
        #[ignore = "needs a directory on a small tmpfs: just shm-exhaustion <dir>"]
        fn on_a_tmpfs_a_removed_files_room_is_there_for_the_next_ask_at_once() {
            // The look refuses on what the filesystem says is free. A tmpfs says so
            // of a removed file's room before the removal returns: a file of more
            // than half the free room is given, removed and asked for again, and the
            // look never refuses the second. XFS and btrfs, which are not looked at,
            // refused it when they were (ADR 0175).
            let (first, probe) = new_file_in(full_dir(), "again-probe");
            let free = free_in_the_full_dir(&probe);
            drop(probe);
            std::fs::remove_file(first).expect("removed");
            let length = free / 4 * 3;
            for round in 0..8 {
                let (path, file) = new_file_in(full_dir(), "again");
                sized(&file, length).unwrap_or_else(|error| panic!("round {round}: {error}"));
                drop(file);
                std::fs::remove_file(path).expect("removed");
            }
        }
    }
}
