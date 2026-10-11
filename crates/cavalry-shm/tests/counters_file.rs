//! The counters file on disk: `CountersFile` creates and writes it, `CountersReader`
//! opens it with the spec's checks, and the file's bytes are the spec's (ADR 0012). The
//! full case comes first (`.claude/rules/data-plane.md`, "Bounds"). Every test works in
//! a directory of its own under the system's temporary directory and removes it after.
//! Miri has no file-backed mapping, so `just miri` skips these and runs the layout,
//! cell and conformance tests.

use std::fs;
use std::path::{Path, PathBuf};

use cavalry_shm::counters::generated::{COUNTERS, COUNTERS_ALLOCATED, COUNTERS_ALLOCATION_FAILED};
use cavalry_shm::counters::layout::{
    FILE_NAME, HEADER_LENGTH, HeaderFields, MAX_LABEL_LENGTH, RECORD_LENGTH, RecordFields,
    SLOT_LENGTH, State, encode_header, encode_record,
};
use cavalry_shm::counters::{CounterId, CountersError, CountersFile, CountersReader, Scope};

/// A fresh directory for one test, removed when the test ends.
struct Dir(PathBuf);

impl Dir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("cavalry-shm-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self) -> PathBuf {
        self.0.join(FILE_NAME)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Any instance type id: `counters.toml` has no instance counter yet, and the file
/// stores a type id without checking it against the table.
const INSTANCE: CounterId = CounterId(7);

/// The records of a file of `capacity` that no static counter occupies, in order: a
/// static counter sits in the record whose index is its id, so the free records need
/// not be contiguous.
fn free_records(capacity: u32) -> Vec<u32> {
    (0..capacity)
        .filter(|record| {
            !COUNTERS
                .iter()
                .any(|def| def.scope == Scope::Static && def.id.record() == *record)
        })
        .collect()
}

fn statics() -> usize {
    COUNTERS
        .iter()
        .filter(|def| def.scope == Scope::Static)
        .count()
}

/// A valid file whose header bytes are then changed by `patch`, for the header checks.
fn file_with_header(dir: &Dir, patch: impl FnOnce(&mut [u8])) {
    let file = CountersFile::create(dir.path(), 64).unwrap();
    drop(file);
    let mut bytes = fs::read(dir.file()).unwrap();
    patch(&mut bytes[..HEADER_LENGTH]);
    fs::write(dir.file(), bytes).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_full_case_before_anything_else() {
    let dir = Dir::new("full");
    let capacity = 64;
    let mut file = CountersFile::create(dir.path(), capacity).unwrap();
    let statics = statics();
    let reader = CountersReader::open(dir.path()).unwrap();
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        statics as i64
    );

    let free = free_records(capacity);
    assert_eq!(free.len(), capacity as usize - statics);
    for (n, &expected) in free.iter().enumerate() {
        let counter = file
            .allocate(
                INSTANCE,
                &[1, 2, 3],
                &format!("publication.back_pressure {n}"),
            )
            .unwrap_or_else(|error| panic!("allocation {n} refused: {error}"));
        assert_eq!(
            counter.id(),
            expected,
            "free records are handed out in order"
        );
        counter.increment();
    }
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        i64::from(capacity)
    );
    assert_eq!(reader.read(COUNTERS_ALLOCATION_FAILED.record()).unwrap(), 0);

    let refused = file.allocate(INSTANCE, &[], "one too many").unwrap_err();
    assert!(
        matches!(refused, CountersError::Full { capacity: 64 }),
        "{refused}"
    );
    assert_eq!(refused.reason(), "full");
    assert!(
        refused
            .to_string()
            .contains("docs/reference/counters-file.md#when-the-file-is-full"),
        "{refused}"
    );
    assert_eq!(
        reader.read(COUNTERS_ALLOCATION_FAILED.record()).unwrap(),
        1,
        "the refusal is counted"
    );
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        i64::from(capacity),
        "nothing was allocated"
    );
    for id in free {
        assert_eq!(reader.read(id).unwrap(), 1, "counter {id} kept its value");
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn create_then_open_round_trips_the_header_and_the_bytes_are_the_specs() {
    let dir = Dir::new("round-trip");
    let file = CountersFile::create(dir.path(), 64).unwrap();
    let header = file.header();
    assert_eq!(header.capacity(), 64);
    assert_eq!(header.creator_pid(), i64::from(std::process::id()));
    assert!(
        header.start_timestamp() > 1_700_000_000_000_000_000,
        "nanoseconds since the epoch"
    );
    assert_eq!(
        fs::metadata(dir.file()).unwrap().len(),
        (HEADER_LENGTH + 64 * (RECORD_LENGTH + SLOT_LENGTH)) as u64
    );

    let reader = CountersReader::open(dir.path()).unwrap();
    assert_eq!(reader.header(), header);
    assert_eq!(reader.capacity(), 64);

    let bytes = fs::read(dir.file()).unwrap();
    assert_eq!(
        &bytes[..HEADER_LENGTH],
        &encode_header(&header)[..],
        "the header bytes are the spec's"
    );
    let record = RecordFields::new(
        State::Allocated,
        COUNTERS_ALLOCATED.0,
        0,
        &[],
        "counters.allocated",
    )
    .unwrap();
    let offset = header.record_offset(COUNTERS_ALLOCATED.record()).unwrap();
    assert_eq!(
        &bytes[offset..offset + RECORD_LENGTH],
        &encode_record(&record)[..],
        "record 0 is the spec's"
    );
    let slot = header.slot_offset(COUNTERS_ALLOCATED.record()).unwrap();
    assert_eq!(
        &bytes[slot..slot + 8],
        &(statics() as i64).to_le_bytes(),
        "slot 0 holds counters.allocated"
    );
    assert!(
        bytes[slot + 8..slot + SLOT_LENGTH].iter().all(|&b| b == 0),
        "the slot's reserved bytes are 0"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn static_counters_are_present_with_their_labels() {
    let dir = Dir::new("statics");
    let file = CountersFile::create(dir.path(), 64).unwrap();
    let reader = CountersReader::open(dir.path()).unwrap();
    for def in COUNTERS.iter().filter(|def| def.scope == Scope::Static) {
        let id = def.id.record();
        let record = reader
            .record(id)
            .unwrap()
            .unwrap_or_else(|| panic!("{} has no record", def.name));
        assert_eq!(record.state(), State::Allocated);
        assert_eq!(record.type_id(), def.id.0);
        assert_eq!(
            record.label(),
            def.name,
            "a static counter's label is its name"
        );
        assert_eq!(
            record.key(),
            &[0u8; 112],
            "a static counter's key is all zero"
        );
        assert_eq!(record.reclaim_deadline(), 0);
        let handle = file.counter(id).unwrap();
        assert_eq!(handle.id(), id);
        assert_eq!(handle.get(), reader.read(id).unwrap());
    }
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        statics() as i64
    );
    assert_eq!(reader.read(COUNTERS_ALLOCATION_FAILED.record()).unwrap(), 0);
    assert!(
        reader.record(free_records(64)[0]).unwrap().is_none(),
        "the first free record is unused"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_handle_moves_the_value_a_reader_sees() {
    let dir = Dir::new("handle");
    let mut file = CountersFile::create(dir.path(), 64).unwrap();
    let reader = CountersReader::open(dir.path()).unwrap();
    let counter = file.allocate(INSTANCE, &[9], "image.gaps 9").unwrap();
    let id = counter.id();
    assert_eq!(reader.read(id).unwrap(), 0, "a new counter is 0");
    counter.increment();
    counter.add(10);
    assert_eq!(reader.read(id).unwrap(), 11);
    assert_eq!(counter.get(), 11);
    counter.set(-3);
    assert_eq!(reader.read(id).unwrap(), -3);

    let record = reader.record(id).unwrap().unwrap();
    assert_eq!(record.type_id(), INSTANCE.0);
    assert_eq!(record.label(), "image.gaps 9");
    assert_eq!(record.key()[0], 9);

    let clone = counter.clone();
    drop(file);
    clone.increment();
    assert_eq!(
        reader.read(id).unwrap(),
        -2,
        "a handle outlives the file handle"
    );
    assert_eq!(format!("{counter:?}"), format!("Counter {{ id: {id} }}"));
}

#[test]
#[cfg_attr(miri, ignore)]
fn too_short_when_the_file_has_no_header() {
    let dir = Dir::new("too-short");
    fs::write(dir.file(), [0u8; 100]).unwrap();
    let error = CountersReader::open(dir.path()).unwrap_err();
    assert!(
        matches!(error, CountersError::TooShort { given: 100 }),
        "{error}"
    );
    assert_eq!(error.reason(), "too-short");
}

#[test]
#[cfg_attr(miri, ignore)]
fn not_ready_while_the_magic_is_zero() {
    let dir = Dir::new("not-ready");
    fs::write(
        dir.file(),
        vec![0u8; HEADER_LENGTH + 64 * (RECORD_LENGTH + SLOT_LENGTH)],
    )
    .unwrap();
    let error = CountersReader::open(dir.path()).unwrap_err();
    assert!(matches!(error, CountersError::NotReady), "{error}");
    assert_eq!(error.reason(), "not-ready");
}

#[test]
#[cfg_attr(miri, ignore)]
fn magic_when_the_file_is_another_layout() {
    let dir = Dir::new("magic");
    file_with_header(&dir, |header| header[..8].copy_from_slice(b"CVLYLOGB"));
    let error = CountersReader::open(dir.path()).unwrap_err();
    assert!(
        matches!(error, CountersError::Magic { found } if &found == b"CVLYLOGB"),
        "{error}"
    );
    assert_eq!(error.reason(), "magic");
    assert!(error.to_string().contains("43564c594c4f4742"), "{error}");
}

#[test]
#[cfg_attr(miri, ignore)]
fn version_when_the_file_is_from_another_cavalry() {
    let dir = Dir::new("version");
    file_with_header(&dir, |header| header[8] = 1);
    let error = CountersReader::open(dir.path()).unwrap_err();
    assert!(
        matches!(error, CountersError::Version { found: 1 }),
        "{error}"
    );
    assert_eq!(error.reason(), "version");
}

#[test]
#[cfg_attr(miri, ignore)]
fn capacity_when_the_header_claims_an_impossible_one() {
    let dir = Dir::new("capacity");
    file_with_header(&dir, |header| {
        header[12..16].copy_from_slice(&100i32.to_le_bytes())
    });
    let error = CountersReader::open(dir.path()).unwrap_err();
    assert!(
        matches!(error, CountersError::Capacity { found: 100 }),
        "{error}"
    );
    assert_eq!(error.reason(), "capacity");
}

#[test]
#[cfg_attr(miri, ignore)]
fn length_when_the_file_is_truncated() {
    let dir = Dir::new("length");
    let file = CountersFile::create(dir.path(), 64).unwrap();
    drop(file);
    let full = fs::metadata(dir.file()).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(dir.file())
        .unwrap()
        .set_len(full - 1)
        .unwrap();
    let error = CountersReader::open(dir.path()).unwrap_err();
    assert!(
        matches!(error, CountersError::Length { found, needed } if found == full - 1 && needed == full),
        "{error}"
    );
    assert_eq!(error.reason(), "length");
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_longer_file_is_fine() {
    let dir = Dir::new("longer");
    let file = CountersFile::create(dir.path(), 64).unwrap();
    drop(file);
    let full = fs::metadata(dir.file()).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(dir.file())
        .unwrap()
        .set_len(full + 4096)
        .unwrap();
    let reader = CountersReader::open(dir.path()).unwrap();
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        statics() as i64
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn id_when_a_counter_is_beyond_the_capacity() {
    let dir = Dir::new("id");
    let mut file = CountersFile::create(dir.path(), 64).unwrap();
    let reader = CountersReader::open(dir.path()).unwrap();
    for error in [
        reader.read(64).unwrap_err(),
        reader.record(64).unwrap_err(),
        file.counter(64).unwrap_err(),
        file.free(64, 0).unwrap_err(),
    ] {
        assert!(
            matches!(
                error,
                CountersError::Id {
                    found: 64,
                    capacity: 64
                }
            ),
            "{error}"
        );
        assert_eq!(error.reason(), "id");
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn unallocated_when_the_record_holds_no_counter() {
    let dir = Dir::new("unallocated");
    let mut file = CountersFile::create(dir.path(), 64).unwrap();
    let reader = CountersReader::open(dir.path()).unwrap();
    let unused = free_records(64)[0];
    assert!(reader.record(unused).unwrap().is_none());
    for error in [
        reader.read(unused).unwrap_err(),
        file.counter(unused).unwrap_err(),
        file.free(unused, 0).unwrap_err(),
    ] {
        assert!(
            matches!(error, CountersError::Unallocated { id } if id == unused),
            "{error}"
        );
        assert_eq!(error.reason(), "unallocated");
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn free_then_reuse_after_the_deadline() {
    let dir = Dir::new("free");
    let mut file = CountersFile::create(dir.path(), 64).unwrap();
    let reader = CountersReader::open(dir.path()).unwrap();
    let statics = statics() as i64;
    let free = free_records(64);
    let (a, b) = (free[0], free[1]);
    let first = file.allocate(INSTANCE, &[1], "image.gaps 1").unwrap();
    assert_eq!(first.id(), a, "the first free record");
    first.add(5);
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        statics + 1
    );

    file.free(a, i64::MAX).unwrap();
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        statics,
        "the gauge drops"
    );
    assert!(
        reader.record(a).unwrap().is_none(),
        "a reclaimed record is not a counter"
    );
    assert!(matches!(reader.read(a).unwrap_err(), CountersError::Unallocated { id } if id == a));
    assert!(
        matches!(file.free(a, 0).unwrap_err(), CountersError::Unallocated { id } if id == a),
        "freeing twice"
    );

    let second = file.allocate(INSTANCE, &[2], "image.gaps 2").unwrap();
    assert_eq!(
        second.id(),
        b,
        "the freed record's deadline is ahead, so the next free record is taken"
    );

    file.free(b, 0).unwrap();
    let third = file.allocate(INSTANCE, &[3], "image.gaps 3").unwrap();
    assert_eq!(
        third.id(),
        b,
        "a reclaimed record whose deadline passed is reused"
    );
    assert_eq!(reader.read(b).unwrap(), 0, "a reused slot starts at 0");
    let record = reader.record(b).unwrap().unwrap();
    assert_eq!(record.label(), "image.gaps 3");
    assert_eq!(record.key()[0], 3);
    assert_eq!(
        record.reclaim_deadline(),
        0,
        "allocation rewrites every field"
    );
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        statics + 1
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn create_refuses_a_file_that_exists_and_an_impossible_capacity() {
    let dir = Dir::new("create");
    let error = CountersFile::create(dir.path(), 100).unwrap_err();
    assert!(
        matches!(error, CountersError::Capacity { found: 100 }),
        "{error}"
    );
    let error = CountersFile::create(dir.path(), 32).unwrap_err();
    assert!(
        matches!(error, CountersError::Capacity { found: 32 }),
        "{error}"
    );
    assert!(!dir.file().exists(), "a refused capacity creates nothing");

    let file = CountersFile::create(dir.path(), 64).unwrap();
    let error = CountersFile::create(dir.path(), 64).unwrap_err();
    assert!(matches!(error, CountersError::Io(_)), "{error}");
    assert_eq!(error.reason(), "io");
    assert!(
        error
            .to_string()
            .contains("docs/reference/counters-file.md#file"),
        "{error}"
    );
    drop(file);

    let error = CountersReader::open(Path::new("/nonexistent/cavalry")).unwrap_err();
    assert!(matches!(error, CountersError::Io(_)), "{error}");
}

#[test]
#[cfg_attr(miri, ignore)]
fn allocate_refuses_a_long_key_or_label_without_touching_the_file() {
    let dir = Dir::new("bounds");
    let mut file = CountersFile::create(dir.path(), 64).unwrap();
    let reader = CountersReader::open(dir.path()).unwrap();
    let error = file.allocate(INSTANCE, &[0u8; 113], "x").unwrap_err();
    assert!(
        matches!(error, CountersError::Key { found: 113 }),
        "{error}"
    );
    assert_eq!(error.reason(), "key");
    let label = "y".repeat(MAX_LABEL_LENGTH + 1);
    let error = file.allocate(INSTANCE, &[], &label).unwrap_err();
    assert!(
        matches!(error, CountersError::Label { found: 125 }),
        "{error}"
    );
    assert_eq!(error.reason(), "label");
    assert_eq!(
        reader.read(COUNTERS_ALLOCATED.record()).unwrap(),
        statics() as i64
    );
    assert_eq!(reader.read(COUNTERS_ALLOCATION_FAILED.record()).unwrap(), 0);
    assert!(reader.record(free_records(64)[0]).unwrap().is_none());

    let widest = file
        .allocate(INSTANCE, &[0u8; 112], &"z".repeat(MAX_LABEL_LENGTH))
        .unwrap();
    assert_eq!(
        reader.record(widest.id()).unwrap().unwrap().label().len(),
        MAX_LABEL_LENGTH
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn every_error_names_the_page_that_explains_it() {
    let errors = [
        CountersError::TooShort { given: 1 },
        CountersError::NotReady,
        CountersError::Magic {
            found: *b"CVLYLOGB",
        },
        CountersError::Version { found: 1 },
        CountersError::Capacity { found: 100 },
        CountersError::Length {
            found: 1,
            needed: 2,
        },
        CountersError::Changed,
        CountersError::Id {
            found: 64,
            capacity: 64,
        },
        CountersError::State { found: 2 },
        CountersError::Label { found: 125 },
        CountersError::Key { found: 113 },
        CountersError::Full { capacity: 64 },
        CountersError::Unallocated { id: 5 },
        CountersError::Io(std::io::Error::other("refused")),
    ];
    let reasons: Vec<&str> = errors.iter().map(CountersError::reason).collect();
    assert_eq!(
        reasons,
        [
            "too-short",
            "not-ready",
            "magic",
            "version",
            "capacity",
            "length",
            "changed",
            "id",
            "state",
            "label",
            "key",
            "full",
            "unallocated",
            "io"
        ]
    );
    for error in &errors {
        let message = error.to_string();
        assert!(
            message.contains(&format!("({})", error.reason())),
            "{message}"
        );
        assert!(
            message.contains("docs/reference/counters-file.md#"),
            "{message}"
        );
    }
    let header = HeaderFields::new(64, 1, 0).unwrap();
    assert_eq!(header.file_length(), 128 + 64 * 384);
}
