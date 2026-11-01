//! Every mapping is made resident when it is made (ADR 0027): after `create` and `open`,
//! each page of the log buffer, the counters file, the command ring and the broadcast
//! buffer is resident in every mapping of it, writable or read-only, so the data plane
//! never takes a page fault. Linux reports residency per mapping in `/proc/self/smaps`;
//! on other systems the touch loop runs but nothing readable says what it did, and the
//! Linux gate is the proof. Miri has no file-backed mapping and no file system, so each
//! test is ignored under `just miri`, as the other file-backed tests are.
#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};

use cavalry_shm::broadcast::BroadcastBuffer;
use cavalry_shm::command_ring::CommandRing;
use cavalry_shm::counters::{CountersFile, CountersReader, CountersWriter};
use cavalry_shm::log::layout::file_name;
use cavalry_shm::log::{HeaderFields, LogBuffers, LogType};

struct Dir(PathBuf);

impl Dir {
    fn new(test: &str) -> Self {
        // /dev/shm when the host has it, as the driver prefers; the temporary directory
        // otherwise. Residency is per mapping on both.
        let base = if Path::new("/dev/shm").is_dir() {
            PathBuf::from("/dev/shm")
        } else {
            std::env::temp_dir()
        };
        let path = base.join(format!(
            "cavalry-shm-pretouch-{}-{test}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One mapping of a file, as `/proc/self/smaps` reports it: its size and its resident
/// set, both in kB.
#[derive(Debug)]
struct Mapping {
    size_kb: u64,
    rss_kb: u64,
}

/// Whether a line of `/proc/self/smaps` begins a mapping: `start-end` in hex, then the
/// permissions, offset, device, inode and, for a file, its path.
fn is_header(line: &str) -> bool {
    line.split_whitespace()
        .next()
        .and_then(|range| range.split_once('-'))
        .is_some_and(|(start, end)| {
            u64::from_str_radix(start, 16).is_ok() && u64::from_str_radix(end, 16).is_ok()
        })
}

/// Every mapping of the file at `path` in this process.
fn mappings_of(path: &Path) -> Vec<Mapping> {
    let smaps = fs::read_to_string("/proc/self/smaps").expect("/proc/self/smaps");
    let wanted = path.to_str().expect("a UTF-8 path");
    let mut found = Vec::new();
    let mut current: Option<Mapping> = None;
    for line in smaps.lines() {
        // A mapping header has the address range first and the path, if any, last.
        if is_header(line) {
            if let Some(done) = current.take() {
                found.push(done);
            }
            if line.split_whitespace().last() == Some(wanted) {
                current = Some(Mapping {
                    size_kb: 0,
                    rss_kb: 0,
                });
            }
            continue;
        }
        if let Some(mapping) = current.as_mut() {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some("Size:"), Some(kb)) => mapping.size_kb = kb.parse().expect("kB"),
                (Some("Rss:"), Some(kb)) => mapping.rss_kb = kb.parse().expect("kB"),
                _ => {}
            }
        }
    }
    if let Some(done) = current.take() {
        found.push(done);
    }
    found
}

fn assert_resident(path: &Path, expected_mappings: usize) {
    let mappings = mappings_of(path);
    assert_eq!(
        mappings.len(),
        expected_mappings,
        "{} mappings of {}: {mappings:?}",
        mappings.len(),
        path.display()
    );
    for mapping in &mappings {
        assert!(
            mapping.size_kb > 0,
            "a mapping of {} with no size",
            path.display()
        );
        assert_eq!(
            mapping.rss_kb,
            mapping.size_kb,
            "a mapping of {} is not fully resident: {mapping:?}",
            path.display()
        );
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_log_buffer_is_resident_in_the_creator_a_publisher_and_a_subscriber() {
    let dir = Dir::new("log");
    let fields = HeaderFields::new(LogType::Exclusive, 7, 0, 65_536, 1408, 32_768, 1, 1001)
        .expect("valid fields");
    let created = LogBuffers::create(dir.path(), &fields).expect("create");
    let publisher = LogBuffers::open(dir.path(), 7, true).expect("open writable");
    let subscriber = LogBuffers::open(dir.path(), 7, false).expect("open read-only");
    assert_resident(&dir.path().join(file_name(7)), 3);
    drop((created, publisher, subscriber));
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_counters_file_is_resident_in_the_driver_a_writer_and_a_reader() {
    let dir = Dir::new("counters");
    let file = CountersFile::create(dir.path(), 128).expect("create");
    let writer = CountersWriter::open(dir.path()).expect("open writable");
    let reader = CountersReader::open(dir.path()).expect("open read-only");
    assert_resident(&dir.path().join("counters"), 3);
    drop((file, writer, reader));
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_command_ring_is_resident_in_the_driver_a_client_and_doctor() {
    let dir = Dir::new("ring");
    let ring = CommandRing::create(dir.path(), 65_536).expect("create");
    let client = CommandRing::open(dir.path(), true).expect("open writable");
    let doctor = CommandRing::open(dir.path(), false).expect("open read-only");
    assert_resident(&dir.path().join("command-ring"), 3);
    drop((ring, client, doctor));
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_broadcast_buffer_is_resident_in_the_driver_and_a_client() {
    let dir = Dir::new("broadcast");
    let buffer = BroadcastBuffer::create(dir.path(), 65_536).expect("create");
    let client = BroadcastBuffer::open(dir.path(), false).expect("open read-only");
    assert_resident(&dir.path().join("broadcast-buffer"), 2);
    drop((buffer, client));
}
