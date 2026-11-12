//! The log recordings file: a cluster member's record of which recordings in its log
//! directory hold its log, in the order they carry it on (`spec/durable-log.md`,
//! version 3, "Log recordings"; ADR 0108).
//!
//! A member's log is in several recordings: one for each time it joined a leader's
//! log, one for each replay it caught up by, one for each term it led. The catalog
//! holds them all, and holds as well the recordings the member made and never took
//! into its log, whose bytes may be of a history the cluster cut. This file is what
//! tells them apart when the member starts again: a recording is part of the member's
//! log once this file names it, and the member reports no position a recording holds
//! before then.
//!
//! The file names at most [`MAX_ENTRIES`] recordings, the newest. Beside them it
//! holds one position, the floor: where the first recording named begins, and, when
//! none is named, where the member's log ends. So a list that names no recording
//! still says where the log ends, which a list alone could not after the member's
//! table has forgotten its oldest recordings and a truncation has purged the rest.
//!
//! The consensus module of `cavalry-raft` is the file's only writer, from its own
//! thread: [`RecordingsFile::open`] at the start of a member whose record of terms
//! holds no term, which creates the file when there is none,
//! [`RecordingsFile::open_kept`] at the start of a member whose record holds a term,
//! which never creates it, and [`RecordingsFile::write`] each time the list changes,
//! which syncs before it returns. The list is written whole, into the slot of two
//! that the list is not read from, under a checksum and a sequence one above the
//! other slot's: a crash tears at most the slot being written, whose checksum then
//! fails, and the reader takes the other, the list as it was.
//! [`RecordingsHeader::parse`], [`parse_slot`] and [`choose`] are the checks of "What
//! a reader rejects", in order, which the vectors of `spec/vectors/durable-log.json`
//! pin.
//!
//! Nothing here runs per message: the file is written when a member joins a log,
//! catches up, leads a term or truncates.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::layout::crc32;

/// The file's name in the log directory.
pub const FILE_NAME: &str = "log-recordings";
/// The header's eight-byte magic, `CVLYLOGR`.
pub const MAGIC: [u8; 8] = *b"CVLYLOGR";
/// The one version this crate writes and reads.
pub const VERSION: i32 = 0;
/// The header's length in bytes.
pub const HEADER_LENGTH: usize = 64;
/// The most recordings a list names: the 1,024 a member keeps once it has stopped
/// them, its recording of a replay and its recording of the live log
/// (`spec/cluster.md`, "Roles and the election", state 0).
pub const MAX_ENTRIES: usize = 1026;
/// An entry's length in bytes: the recording id and its start position.
pub const ENTRY_LENGTH: usize = 16;
/// The length of a slot's own fields, before its entries: the checksum, the entry
/// count, the sequence and the floor position.
pub const SLOT_HEADER_LENGTH: usize = 24;
/// A slot's length in bytes: its fields and room for [`MAX_ENTRIES`] entries.
pub const SLOT_LENGTH: usize = SLOT_HEADER_LENGTH + MAX_ENTRIES * ENTRY_LENGTH;
/// The file's length in bytes, fixed: the header and two slots.
pub const FILE_LENGTH: usize = HEADER_LENGTH + 2 * SLOT_LENGTH;
/// A position's alignment: a frame's (`spec/log-buffer.md`, "Positions").
const ALIGNMENT: i64 = 32;

/// Why a log recordings file, or one of its slots, was refused: the checks of
/// `spec/durable-log.md`, "What a reader rejects", by name, and the I/O errors around
/// them.
#[derive(Debug)]
pub enum RecordingsError {
    /// The file could not be opened, read, written or synced.
    Io {
        /// What was being done.
        what: &'static str,
        /// The file.
        path: PathBuf,
        /// The operating system's reason.
        source: io::Error,
    },
    /// The directory holds no file, and [`RecordingsFile::open_kept`] creates none:
    /// the member's record of terms holds a term, so a list created now, empty, would
    /// say that the member holds no log.
    Absent {
        /// Where the file would be.
        path: PathBuf,
    },
    /// The file is shorter than its fixed length.
    TooShort {
        /// The length found.
        found: u64,
    },
    /// The file is longer than its fixed length.
    Length {
        /// The length found.
        found: u64,
    },
    /// The magic is eight zero bytes: the creator has not finished.
    /// [`RecordingsFile::open`] writes a torn creation again, and returns this only
    /// for a zero magic over a list, which creation never writes: damage.
    /// [`RecordingsFile::open_kept`] returns it for every zero magic.
    NotReady,
    /// The magic is not `CVLYLOGR`.
    Magic,
    /// The version is not 0.
    Version {
        /// The version found.
        found: i32,
    },
    /// The slot length is not [`SLOT_LENGTH`].
    SlotLength {
        /// The slot length found.
        found: i32,
    },
    /// The member id is below 0, or is not the id of the member that opened the file:
    /// the directory is another member's.
    Member {
        /// The member id the file holds.
        found: i32,
        /// The id of the member that opened it, `None` when the header alone was read.
        expected: Option<i32>,
    },
    /// A slot's checksum does not match its bytes, or its entry count is not 0 to
    /// [`MAX_ENTRIES`], so that no checksum can be taken: the slot a crash tore, or
    /// the slot never written. One such slot is not an error of the file's; both are
    /// [`RecordingsError::Slots`].
    Checksum,
    /// Neither slot's checksum matches: damage, since a crash tears at most one slot.
    Slots,
    /// A slot whose checksum matches holds a sequence below 0, or both slots hold the
    /// same sequence: damage, since each write's sequence is one above the other
    /// slot's. [`RecordingsFile::write`] returns it too, and writes nothing, for a
    /// file whose list holds the highest sequence an i64 has, which no count of
    /// writes reaches.
    Sequence {
        /// The sequence found.
        found: i64,
    },
    /// An entry's recording id is below 0.
    Recording {
        /// The recording id found.
        found: i64,
    },
    /// The floor position is below 0 or not a multiple of 32; or an entry's start
    /// position is below 0, not a multiple of 32, or below the start position of the
    /// entry before it; or the first entry's start position is not the floor
    /// position.
    Position {
        /// The position found.
        found: i64,
    },
    /// A list of more than [`MAX_ENTRIES`] recordings was given to
    /// [`RecordingsFile::write`], which wrote nothing.
    Full,
}

impl RecordingsError {
    /// The check's name as the spec and the vectors spell it.
    pub const fn reason(&self) -> &'static str {
        match self {
            RecordingsError::Io { .. } => "io",
            RecordingsError::Absent { .. } => "absent",
            RecordingsError::TooShort { .. } => "too-short",
            RecordingsError::Length { .. } => "length",
            RecordingsError::NotReady => "not-ready",
            RecordingsError::Magic => "magic",
            RecordingsError::Version { .. } => "version",
            RecordingsError::SlotLength { .. } => "slot-length",
            RecordingsError::Member { .. } => "member",
            RecordingsError::Checksum => "checksum",
            RecordingsError::Slots => "slots",
            RecordingsError::Sequence { .. } => "sequence",
            RecordingsError::Recording { .. } => "recording",
            RecordingsError::Position { .. } => "position",
            RecordingsError::Full => "log-recordings-full",
        }
    }
}

const DOC: &str = "docs/reference/durable-log.md#log-recordings";

impl std::fmt::Display for RecordingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // What an operator does when the member cannot tell its log: never a new log
        // directory for this member alone (`spec/cluster.md`, "A member's start").
        let stay_out = "Without the list this member cannot tell its log from the other \
                        recordings in the directory, and it does not start. Leave it \
                        stopped: the cluster goes on while a quorum of the others runs. To \
                        have every member again, start the cluster again as a new cluster, \
                        every member on a new log directory. Do not start this member \
                        alone on a new log directory: it would vote with no record of its \
                        votes or its log";
        match self {
            RecordingsError::Io { what, path, source } => write!(
                f,
                "could not {what} the log recordings file {}: {source}. Check that the \
                 log directory exists, is writable by this user and has free space; see \
                 {DOC}",
                path.display()
            ),
            RecordingsError::Absent { path } => write!(
                f,
                "the log directory holds a term in its record of terms and no log \
                 recordings file, {}: a version of Cavalry before this one wrote the \
                 directory, or the file was removed. {stay_out}; see {DOC}",
                path.display()
            ),
            RecordingsError::TooShort { found } => write!(
                f,
                "the log recordings file is {found} bytes, shorter than its {FILE_LENGTH}: \
                 the file is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Length { found } => write!(
                f,
                "the log recordings file is {found} bytes, longer than its {FILE_LENGTH}: \
                 the file is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::NotReady => write!(
                f,
                "the log recordings file has no magic, over a list of recordings or \
                 beside a record of terms that holds a term, which its creation never \
                 leaves: the file is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Magic => write!(
                f,
                "the log recordings file does not begin with CVLYLOGR: it is not a log \
                 recordings file, or it is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Version { found } => write!(
                f,
                "the log recordings file is version {found}, and this version of Cavalry \
                 reads version {VERSION}: start the member with the version of Cavalry \
                 that wrote the directory; see {DOC}"
            ),
            RecordingsError::SlotLength { found } => write!(
                f,
                "the log recordings file names a slot length of {found}, not \
                 {SLOT_LENGTH}: the file is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Member {
                found,
                expected: Some(expected),
            } => write!(
                f,
                "the log directory is member {found}'s, and this is member {expected}: \
                 start member {expected} with its own log directory, the one it was \
                 started with before; see {DOC}"
            ),
            RecordingsError::Member {
                found,
                expected: None,
            } => write!(
                f,
                "the log recordings file names member {found}, which is no member id: \
                 the file is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Checksum => write!(
                f,
                "a slot of the log recordings file fails its checksum: the write a crash \
                 tore, which the other slot stands in for. Nothing is to be done; see \
                 {DOC}"
            ),
            RecordingsError::Slots => write!(
                f,
                "neither slot of the log recordings file passes its checksum, and a \
                 crash tears one at most: the file is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Sequence { found } => write!(
                f,
                "a slot of the log recordings file holds the sequence {found}, which is \
                 below 0, the other slot's own, or the highest there is, which no write \
                 reaches: the file is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Recording { found } => write!(
                f,
                "the log recordings file names the recording {found}, which is no \
                 recording id: the file is damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Position { found } => write!(
                f,
                "the log recordings file holds the position {found}, which is below 0, \
                 not a multiple of 32, below the start of the recording before it, or \
                 the first recording's start and not the floor position: the file is \
                 damaged. {stay_out}; see {DOC}"
            ),
            RecordingsError::Full => write!(
                f,
                "a member's log is in more than {MAX_ENTRIES} recordings, more than the \
                 log recordings file holds; nothing was written. This is a fault in \
                 Cavalry: report it with the member's counters; see {DOC}"
            ),
        }
    }
}

impl std::error::Error for RecordingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RecordingsError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// One recording of the member's log, as the list names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListedRecording {
    /// The recording's id in the catalog.
    pub recording_id: i64,
    /// The position its first frame is at, the catalog's start position for it.
    pub start_position: i64,
}

/// The file's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingsHeader {
    /// The member whose log the file lists.
    pub member_id: i32,
}

impl RecordingsHeader {
    /// The header's 64 bytes.
    pub fn encode(&self) -> [u8; HEADER_LENGTH] {
        let mut bytes = [0u8; HEADER_LENGTH];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&(SLOT_LENGTH as i32).to_le_bytes());
        bytes[16..20].copy_from_slice(&self.member_id.to_le_bytes());
        bytes
    }

    /// Reads a header from `bytes`, the file being `file_length` bytes long: the
    /// checks `too-short`, `length`, `not-ready`, `magic`, `version`, `slot-length`
    /// and `member`, in that order.
    pub fn parse(bytes: &[u8], file_length: u64) -> Result<Self, RecordingsError> {
        if file_length < FILE_LENGTH as u64 {
            return Err(RecordingsError::TooShort { found: file_length });
        }
        if file_length > FILE_LENGTH as u64 {
            return Err(RecordingsError::Length { found: file_length });
        }
        let Some(header) = bytes.first_chunk::<HEADER_LENGTH>() else {
            return Err(RecordingsError::TooShort {
                found: bytes.len() as u64,
            });
        };
        if header[..8] == [0u8; 8] {
            return Err(RecordingsError::NotReady);
        }
        if header[..8] != MAGIC {
            return Err(RecordingsError::Magic);
        }
        let version = i32_at(header, 8);
        if version != VERSION {
            return Err(RecordingsError::Version { found: version });
        }
        let slot_length = i32_at(header, 12);
        if slot_length != SLOT_LENGTH as i32 {
            return Err(RecordingsError::SlotLength { found: slot_length });
        }
        let member_id = i32_at(header, 16);
        if member_id < 0 {
            return Err(RecordingsError::Member {
                found: member_id,
                expected: None,
            });
        }
        Ok(Self { member_id })
    }
}

fn i32_at(bytes: &[u8], at: usize) -> i32 {
    bytes
        .get(at..at + 4)
        .and_then(|four| four.first_chunk::<4>())
        .map_or(0, |four| i32::from_le_bytes(*four))
}

fn i64_at(bytes: &[u8], at: usize) -> i64 {
    bytes
        .get(at..at + 8)
        .and_then(|eight| eight.first_chunk::<8>())
        .map_or(0, |eight| i64::from_le_bytes(*eight))
}

/// A slot's own fields, after its checksum and its entry count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotFields {
    /// The list's sequence: 0 for the list the file was created with, and one more
    /// for each write.
    pub sequence: i64,
    /// Where the first recording the list names begins, and, when it names none,
    /// where the member's log ends.
    pub floor_position: i64,
}

/// Writes a slot into `out`: its fields, the entries and the checksum over them.
/// Returns how many bytes the slot's fields and entries take, which is what a write
/// puts in the file; the rest of the slot is not read. More than [`MAX_ENTRIES`]
/// entries are [`RecordingsError::Full`], and a floor position or an entry a reader
/// would refuse is the reader's refusal, with nothing written.
pub fn encode_slot(
    fields: SlotFields,
    entries: impl IntoIterator<Item = ListedRecording>,
    out: &mut [u8; SLOT_LENGTH],
) -> Result<usize, RecordingsError> {
    check_floor(fields.floor_position)?;
    let mut count = 0usize;
    let mut previous = fields.floor_position;
    for entry in entries {
        if count == MAX_ENTRIES {
            return Err(RecordingsError::Full);
        }
        check_entry(&entry, previous, count == 0)?;
        previous = entry.start_position;
        let at = SLOT_HEADER_LENGTH + count * ENTRY_LENGTH;
        if let Some(bytes) = out.get_mut(at..at + ENTRY_LENGTH) {
            bytes[..8].copy_from_slice(&entry.recording_id.to_le_bytes());
            bytes[8..].copy_from_slice(&entry.start_position.to_le_bytes());
        }
        count += 1;
    }
    let length = SLOT_HEADER_LENGTH + count * ENTRY_LENGTH;
    out[4..8].copy_from_slice(&(count as i32).to_le_bytes());
    out[8..16].copy_from_slice(&fields.sequence.to_le_bytes());
    out[16..24].copy_from_slice(&fields.floor_position.to_le_bytes());
    let checksum = slot_checksum(out, count);
    out[..4].copy_from_slice(&checksum.to_le_bytes());
    Ok(length)
}

/// The checksum of a slot that holds `count` entries: the zlib CRC32 of everything
/// after the checksum itself, the entry count, the sequence, the floor position and
/// the entries, bytes 4 to 24 + 16 × count.
fn slot_checksum(slot: &[u8], count: usize) -> u32 {
    crc32(
        slot.get(4..SLOT_HEADER_LENGTH + count * ENTRY_LENGTH)
            .unwrap_or(&[]),
    )
}

fn check_floor(floor_position: i64) -> Result<(), RecordingsError> {
    if floor_position < 0 || floor_position % ALIGNMENT != 0 {
        return Err(RecordingsError::Position {
            found: floor_position,
        });
    }
    Ok(())
}

/// An entry's checks. `previous` is the start position of the entry before it, and
/// for the first entry the floor position, which the first entry begins at.
fn check_entry(entry: &ListedRecording, previous: i64, first: bool) -> Result<(), RecordingsError> {
    if entry.recording_id < 0 {
        return Err(RecordingsError::Recording {
            found: entry.recording_id,
        });
    }
    let start = entry.start_position;
    if start < 0 || start % ALIGNMENT != 0 || start < previous || (first && start != previous) {
        return Err(RecordingsError::Position { found: start });
    }
    Ok(())
}

/// Reads a slot from `bytes`, which begin at the slot's first byte and hold at least
/// its fields and its entries; `entries` is emptied and takes the slot's entries.
/// Returns the slot's fields. The checks are `checksum`, which an entry count outside
/// 0 to [`MAX_ENTRIES`] or bytes shorter than the count needs also fail, then
/// `sequence`, then `position` for the floor position, then `recording` and
/// `position` for each entry in order.
pub fn parse_slot(
    bytes: &[u8],
    entries: &mut Vec<ListedRecording>,
) -> Result<SlotFields, RecordingsError> {
    entries.clear();
    if bytes.len() < SLOT_HEADER_LENGTH {
        return Err(RecordingsError::Checksum);
    }
    let count = i32_at(bytes, 4);
    let Some(count) = usize::try_from(count)
        .ok()
        .filter(|count| *count <= MAX_ENTRIES)
    else {
        return Err(RecordingsError::Checksum);
    };
    if bytes.len() < SLOT_HEADER_LENGTH + count * ENTRY_LENGTH {
        return Err(RecordingsError::Checksum);
    }
    let stored = u32::from_le_bytes(bytes.first_chunk::<4>().copied().unwrap_or_default());
    if stored != slot_checksum(bytes, count) {
        return Err(RecordingsError::Checksum);
    }
    let sequence = i64_at(bytes, 8);
    if sequence < 0 {
        return Err(RecordingsError::Sequence { found: sequence });
    }
    let floor_position = i64_at(bytes, 16);
    check_floor(floor_position)?;
    let mut previous = floor_position;
    for index in 0..count {
        let at = SLOT_HEADER_LENGTH + index * ENTRY_LENGTH;
        let entry = ListedRecording {
            recording_id: i64_at(bytes, at),
            start_position: i64_at(bytes, at + 8),
        };
        check_entry(&entry, previous, index == 0)?;
        previous = entry.start_position;
        entries.push(entry);
    }
    Ok(SlotFields {
        sequence,
        floor_position,
    })
}

/// Which slot holds the list, from what [`parse_slot`] made of each: the one whose
/// checksum matches, and of two the one with the higher sequence. A slot whose
/// checksum fails is not read. Neither matching is `slots`; a slot whose checksum
/// matches and whose fields fail a check is that check's refusal, whatever the other
/// slot holds, and slot 0's when both do; two slots of one sequence are `sequence`.
pub fn choose(
    slot_0: Result<SlotFields, RecordingsError>,
    slot_1: Result<SlotFields, RecordingsError>,
) -> Result<usize, RecordingsError> {
    match (slot_0, slot_1) {
        (Err(RecordingsError::Checksum), Err(RecordingsError::Checksum)) => {
            Err(RecordingsError::Slots)
        }
        (Ok(_), Err(RecordingsError::Checksum)) => Ok(0),
        (Err(RecordingsError::Checksum), Ok(_)) => Ok(1),
        (Err(RecordingsError::Checksum), Err(error)) | (Err(error), _) | (_, Err(error)) => {
            Err(error)
        }
        (Ok(first), Ok(second)) if first.sequence == second.sequence => {
            Err(RecordingsError::Sequence {
                found: first.sequence,
            })
        }
        (Ok(first), Ok(second)) => Ok(usize::from(second.sequence > first.sequence)),
    }
}

/// Where slot `slot` begins in the file.
const fn slot_offset(slot: usize) -> u64 {
    (HEADER_LENGTH + slot * SLOT_LENGTH) as u64
}

/// The file, open, with the list it holds in memory. One writer, the consensus
/// module; every write syncs before it returns.
#[derive(Debug)]
pub struct RecordingsFile {
    path: PathBuf,
    file: File,
    /// The slot the list is read from, and its fields.
    slot: usize,
    fields: SlotFields,
    /// The list, at most [`MAX_ENTRIES`], allocated when the file is opened.
    entries: Vec<ListedRecording>,
    /// One slot's bytes, allocated when the file is opened, so that a write
    /// allocates nothing.
    scratch: Box<[u8; SLOT_LENGTH]>,
    /// Whether this open wrote the file, new or over a torn creation.
    created: bool,
}

impl RecordingsFile {
    /// The file's path in `dir`.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(FILE_NAME)
    }

    /// Opens member `member_id`'s file in `dir` and reads its list, and creates the
    /// file with an empty list and a floor position of 0 when there is none: for a
    /// member whose record of terms holds no term, which holds no log. A file a crash
    /// tore while it was created, shorter than its header or with a zero magic over
    /// no list, is written again; a zero magic over a list is
    /// [`RecordingsError::NotReady`]. A file of another member's is
    /// [`RecordingsError::Member`]. The file's length is checked before a byte is
    /// read, and what is read is its fixed length at most.
    pub fn open(dir: &Path, member_id: i32) -> Result<Self, RecordingsError> {
        Self::open_with(dir, member_id, true)
    }

    /// Opens member `member_id`'s file in `dir` and reads its list, and never
    /// creates or writes it: for a member whose record of terms holds a term. Such a
    /// member holds a log, and a file created for it now would say that it holds
    /// none. No file is [`RecordingsError::Absent`]; a file a crash tore while it was
    /// created is [`RecordingsError::TooShort`] or [`RecordingsError::NotReady`],
    /// since creation ends before a member records its first term. Every other
    /// refusal is [`RecordingsFile::open`]'s.
    pub fn open_kept(dir: &Path, member_id: i32) -> Result<Self, RecordingsError> {
        Self::open_with(dir, member_id, false)
    }

    fn open_with(dir: &Path, member_id: i32, create: bool) -> Result<Self, RecordingsError> {
        let path = Self::path_in(dir);
        let io = |what: &'static str| {
            let path = path.clone();
            move |source| RecordingsError::Io { what, path, source }
        };
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .open(&path)
        {
            Ok(file) => file,
            Err(source) if !create && source.kind() == io::ErrorKind::NotFound => {
                return Err(RecordingsError::Absent { path });
            }
            Err(source) => return Err(io("open")(source)),
        };
        let file_length = file.metadata().map_err(io("stat"))?.len();
        if file_length > FILE_LENGTH as u64 {
            return Err(RecordingsError::Length { found: file_length });
        }
        let mut bytes = vec![0u8; FILE_LENGTH];
        let held = usize::try_from(file_length).unwrap_or(FILE_LENGTH);
        file.read_exact_at(bytes.get_mut(..held).unwrap_or(&mut []), 0)
            .map_err(io("read"))?;
        let mut lists = [
            Vec::with_capacity(MAX_ENTRIES),
            Vec::with_capacity(MAX_ENTRIES),
        ];
        let [list_0, list_1] = &mut lists;
        let slots = [
            parse_slot(bytes.get(HEADER_LENGTH..).unwrap_or(&[]), list_0),
            parse_slot(
                bytes.get(HEADER_LENGTH + SLOT_LENGTH..).unwrap_or(&[]),
                list_1,
            ),
        ];
        // A creation a crash tore: creation writes one list, the empty one of
        // sequence 0 and floor 0, and no other. So under a zero magic a slot is the
        // creation's when its checksum fails, the slot no write reached, or when it
        // holds that list; any other slot whose checksum matches, a list a write
        // made or one that fails a later check, is damage and not a creation.
        let creations =
            |slot: &Result<SlotFields, RecordingsError>, list: &[ListedRecording]| match slot {
                Err(RecordingsError::Checksum) => true,
                Ok(fields) => *fields == SlotFields::CREATED && list.is_empty(),
                Err(_) => false,
            };
        let zero_magic = bytes.get(..8).is_some_and(|magic| magic == [0u8; 8]);
        let torn_creation = file_length < HEADER_LENGTH as u64
            || (zero_magic && creations(&slots[0], &lists[0]) && creations(&slots[1], &lists[1]));
        let mut scratch = Box::new([0u8; SLOT_LENGTH]);
        if torn_creation && create {
            Self::create(&file, dir, &path, member_id, &mut scratch)?;
            let [mut entries, _] = lists;
            entries.clear();
            return Ok(Self {
                path,
                file,
                slot: 0,
                fields: SlotFields::CREATED,
                entries,
                scratch,
                created: true,
            });
        }
        let header = RecordingsHeader::parse(&bytes, file_length)?;
        if header.member_id != member_id {
            return Err(RecordingsError::Member {
                found: header.member_id,
                expected: Some(member_id),
            });
        }
        let [slot_0, slot_1] = slots;
        let read = [slot_0.as_ref().ok().copied(), slot_1.as_ref().ok().copied()];
        let slot = choose(slot_0, slot_1)?;
        let [list_0, list_1] = lists;
        Ok(Self {
            path,
            file,
            slot,
            fields: read
                .get(slot)
                .copied()
                .flatten()
                .unwrap_or(SlotFields::CREATED),
            entries: if slot == 0 { list_0 } else { list_1 },
            scratch,
            created: false,
        })
    }

    /// Writes the created file over whatever `file` holds, the way the terms file is
    /// created: the file emptied, the header after its magic, slot 0 as the empty list
    /// of sequence 0 and the file's full length, synced; then the magic, synced; then
    /// the directory `dir`, synced, so that a power loss does not take the file's
    /// name from a directory in which the member has since recorded a term.
    fn create(
        file: &File,
        dir: &Path,
        path: &Path,
        member_id: i32,
        scratch: &mut [u8; SLOT_LENGTH],
    ) -> Result<(), RecordingsError> {
        let io = |what: &'static str| {
            let path = path.to_path_buf();
            move |source| RecordingsError::Io { what, path, source }
        };
        let header = RecordingsHeader { member_id }.encode();
        let length = encode_slot(SlotFields::CREATED, [], scratch)?;
        file.set_len(0).map_err(io("empty"))?;
        file.write_all_at(&header[8..], 8)
            .map_err(io("write the header of"))?;
        file.write_all_at(scratch.get(..length).unwrap_or(&[]), slot_offset(0))
            .map_err(io("write the first list of"))?;
        file.set_len(FILE_LENGTH as u64).map_err(io("extend"))?;
        file.sync_all().map_err(io("sync"))?;
        file.write_all_at(&header[..8], 0)
            .map_err(io("write the magic of"))?;
        file.sync_all().map_err(io("sync"))?;
        File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(io("sync the directory of"))
    }

    /// Whether this open wrote the file: there was none, or a crash had torn the one
    /// a start before this one was creating. The list is then the one a creation
    /// writes, of no recording, which the member counts as it counts each list it
    /// writes (`cluster.log_recordings_listed`).
    pub const fn was_created(&self) -> bool {
        self.created
    }

    /// The list: the recordings that hold the member's log, in the order they carry
    /// it on.
    pub fn entries(&self) -> &[ListedRecording] {
        &self.entries
    }

    /// The floor position: where the first recording of the list begins, and, when
    /// the list names none, where the member's log ends.
    pub const fn floor_position(&self) -> i64 {
        self.fields.floor_position
    }

    /// The sequence of the list the file holds: 0 for the list it was created with,
    /// and one more for each write.
    pub const fn sequence(&self) -> i64 {
        self.fields.sequence
    }

    /// Replaces the list with `entries` over `floor_position` and syncs: one write of
    /// the slot the list is not read from, with the next sequence, and one sync. A
    /// crash before the sync has ended leaves the list as it was. A list a reader
    /// would refuse, or one of more than [`MAX_ENTRIES`], is refused and nothing is
    /// written, and so is any list for a file that holds the highest sequence there
    /// is ([`RecordingsError::Sequence`]). On an error the list in memory is the one
    /// before the call, as the file's is.
    pub fn write(
        &mut self,
        floor_position: i64,
        entries: impl IntoIterator<Item = ListedRecording> + Clone,
    ) -> Result<(), RecordingsError> {
        let Some(sequence) = self.fields.sequence.checked_add(1) else {
            return Err(RecordingsError::Sequence {
                found: self.fields.sequence,
            });
        };
        let fields = SlotFields {
            sequence,
            floor_position,
        };
        let length = encode_slot(fields, entries.clone(), &mut self.scratch)?;
        let next = 1 - self.slot;
        let io = |what: &'static str, path: &Path| {
            let path = path.to_path_buf();
            move |source| RecordingsError::Io { what, path, source }
        };
        self.file
            .write_all_at(self.scratch.get(..length).unwrap_or(&[]), slot_offset(next))
            .map_err(io("write", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        self.slot = next;
        self.fields = fields;
        self.entries.clear();
        self.entries.extend(entries);
        Ok(())
    }
}

impl SlotFields {
    /// The fields of the list a file is created with: sequence 0, and a floor
    /// position of 0, the end of a log that holds nothing.
    pub const CREATED: SlotFields = SlotFields {
        sequence: 0,
        floor_position: 0,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(test: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cavalry-log-recordings-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a directory");
        path
    }

    fn listed(recording_id: i64, start_position: i64) -> ListedRecording {
        ListedRecording {
            recording_id,
            start_position,
        }
    }

    #[test]
    fn a_new_file_holds_an_empty_list_and_a_written_list_is_read_back() {
        let dir = dir("round-trip");
        let mut file = RecordingsFile::open(&dir, 2).expect("the file is created");
        assert_eq!(file.entries(), &[]);
        assert_eq!(file.sequence(), 0);
        assert_eq!(file.floor_position(), 0);
        assert_eq!(
            std::fs::metadata(RecordingsFile::path_in(&dir))
                .expect("the file")
                .len(),
            FILE_LENGTH as u64
        );
        let first = [listed(0, 0)];
        file.write(0, first).expect("the list is written");
        let second = [listed(0, 0), listed(3, 192), listed(4, 160)];
        // An entry that begins below the one before it is refused, and nothing changes.
        assert_eq!(
            file.write(0, second)
                .expect_err("an entry out of order")
                .reason(),
            "position"
        );
        assert_eq!(file.entries(), &first);
        let second = [listed(0, 0), listed(3, 160), listed(4, 192)];
        file.write(0, second).expect("the list is written");
        assert_eq!(file.entries(), &second);
        assert_eq!(file.sequence(), 2);
        drop(file);
        let file = RecordingsFile::open(&dir, 2).expect("the file opens");
        assert_eq!(file.entries(), &second);
        assert_eq!(file.sequence(), 2);
        assert_eq!(file.floor_position(), 0);
        // Another member's directory is refused, with both ids.
        let refused = RecordingsFile::open(&dir, 1).expect_err("another member's");
        assert_eq!(refused.reason(), "member");
        assert!(refused.to_string().contains("member 2's"), "{refused}");
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn the_floor_is_where_the_first_recording_begins_or_where_an_empty_list_ends() {
        let dir = dir("floor");
        let mut file = RecordingsFile::open(&dir, 0).expect("the file is created");
        // The table forgot the recordings below 320: the list begins there.
        let list = [listed(7, 320), listed(8, 320), listed(9, 416)];
        file.write(320, list).expect("the list is written");
        assert_eq!(file.floor_position(), 320);
        // A first recording that does not begin at the floor is refused, above it
        // and below it, as is a floor that is no position; nothing changes.
        for (floor, entries) in [
            (288, &list[..]),
            (352, &list[..]),
            (-32, &[][..]),
            (48, &[][..]),
        ] {
            assert_eq!(
                file.write(floor, entries.iter().copied())
                    .expect_err("a floor the list does not begin at")
                    .reason(),
                "position",
                "floor {floor}"
            );
        }
        assert_eq!(file.entries(), &list);
        // A truncation purged every recording named: the list is empty, and the
        // floor says where the log ends, in recordings the table forgot.
        file.write(256, []).expect("the empty list is written");
        assert_eq!(file.entries(), &[]);
        assert_eq!(file.floor_position(), 256);
        drop(file);
        let file = RecordingsFile::open_kept(&dir, 0).expect("the file opens");
        assert_eq!(file.entries(), &[]);
        assert_eq!(file.floor_position(), 256);
        assert_eq!(file.sequence(), 2);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_torn_write_reads_as_the_list_before_it() {
        let dir = dir("torn-write");
        let mut file = RecordingsFile::open(&dir, 0).expect("the file is created");
        file.write(0, [listed(0, 0)]).expect("the list is written");
        file.write(0, [listed(0, 0), listed(1, 96)])
            .expect("the list is written");
        // Sequence 2 is in slot 0, sequence 1 in slot 1. A crash tore the write of
        // sequence 3 into slot 1: its count and sequence are there, its entries not.
        drop(file);
        let path = RecordingsFile::path_in(&dir);
        let raw = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the file");
        let mut torn = [0u8; 12];
        torn[..4].copy_from_slice(&3i32.to_le_bytes());
        torn[4..].copy_from_slice(&3i64.to_le_bytes());
        raw.write_all_at(&torn, slot_offset(1) + 4)
            .expect("a write");
        drop(raw);
        let mut file = RecordingsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(file.entries(), &[listed(0, 0), listed(1, 96)]);
        assert_eq!(file.sequence(), 2);
        // The next write goes over the torn slot.
        file.write(0, [listed(0, 0), listed(1, 96), listed(2, 96)])
            .expect("the list is written");
        drop(file);
        let file = RecordingsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(file.entries().len(), 3);
        assert_eq!(file.sequence(), 3);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_torn_creation_is_written_again_and_damage_is_refused() {
        let dir = dir("damage");
        let path = RecordingsFile::path_in(&dir);
        // A creation the crash took before its magic: the file is created again.
        std::fs::write(&path, vec![0u8; FILE_LENGTH]).expect("a file of zeros");
        let file = RecordingsFile::open(&dir, 1).expect("the file is created again");
        assert_eq!(file.entries(), &[]);
        assert!(file.was_created());
        drop(file);
        assert!(!RecordingsFile::open(&dir, 1).expect("whole").was_created());
        let raw = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the file");
        // A creation the crash took between its two syncs: everything but the
        // magic, with the list a creation writes in slot 0. Created again.
        raw.write_all_at(&[0u8; 8], 0).expect("a write");
        let file = RecordingsFile::open(&dir, 1).expect("the file is created again");
        assert!(file.was_created());
        drop(file);
        // A zero magic over a slot whose checksum matches and which is not that
        // list is damage, though the slot fails a later check: here a floor
        // position of 48, which is no multiple of 32. Refused, and left as it was.
        let mut slot = [0u8; SLOT_HEADER_LENGTH];
        slot[16..24].copy_from_slice(&48i64.to_le_bytes());
        let checksum = crc32(&slot[4..]);
        slot[..4].copy_from_slice(&checksum.to_le_bytes());
        raw.write_all_at(&[0u8; 8], 0).expect("a write");
        raw.write_all_at(&slot, slot_offset(0)).expect("a write");
        let before = std::fs::read(&path).expect("the file");
        assert_eq!(
            RecordingsFile::open(&dir, 1)
                .expect_err("a zero magic over a slot no creation writes")
                .reason(),
            "not-ready"
        );
        assert_eq!(std::fs::read(&path).expect("the file"), before);
        // The same slot under a magic is its own check's refusal.
        raw.write_all_at(&MAGIC, 0).expect("a write");
        assert_eq!(
            RecordingsFile::open(&dir, 1)
                .expect_err("a floor position that is no position")
                .reason(),
            "position"
        );
        drop(raw);
        std::fs::remove_file(&path).expect("the file is removed");
        let mut file = RecordingsFile::open(&dir, 1).expect("the file is created");
        file.write(0, [listed(5, 0)]).expect("the list is written");
        drop(file);
        // A zero magic over a list is damage: creation never writes one.
        let raw = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the file");
        raw.write_all_at(&[0u8; 8], 0).expect("a write");
        assert_eq!(
            RecordingsFile::open(&dir, 1)
                .expect_err("a zero magic over a list")
                .reason(),
            "not-ready"
        );
        raw.write_all_at(&MAGIC, 0).expect("a write");
        assert_eq!(
            RecordingsFile::open(&dir, 1)
                .expect("whole")
                .entries()
                .len(),
            1
        );
        // Both slots torn: damage.
        raw.write_all_at(&[0xFF; 4], slot_offset(0))
            .expect("a write");
        raw.write_all_at(&[0xFF; 4], slot_offset(1))
            .expect("a write");
        assert_eq!(
            RecordingsFile::open(&dir, 1)
                .expect_err("neither slot whole")
                .reason(),
            "slots"
        );
        // A file cut short, and one with bytes after its end.
        raw.set_len(FILE_LENGTH as u64 - 1).expect("a length");
        assert_eq!(
            RecordingsFile::open(&dir, 1)
                .expect_err("cut short")
                .reason(),
            "too-short"
        );
        raw.set_len(FILE_LENGTH as u64 + 1).expect("a length");
        assert_eq!(
            RecordingsFile::open(&dir, 1)
                .expect_err("too long")
                .reason(),
            "length"
        );
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_member_that_holds_a_log_is_given_no_new_list() {
        let dir = dir("kept");
        let path = RecordingsFile::path_in(&dir);
        // No file: nothing is created, and the refusal says what to do.
        let absent = RecordingsFile::open_kept(&dir, 1).expect_err("no file");
        assert_eq!(absent.reason(), "absent");
        assert!(!path.exists(), "the refusal created the file");
        let text = absent.to_string();
        assert!(
            text.contains("every member on a new log directory"),
            "{text}"
        );
        assert!(
            text.contains("Do not start this member alone on a new log directory"),
            "{text}"
        );
        assert!(
            text.contains("docs/reference/durable-log.md#log-recordings"),
            "{text}"
        );
        // A creation a crash tore, cut short or whole with no magic: refused, and
        // left as it was, where `open` would write it again.
        std::fs::write(&path, [0u8; 40]).expect("a short file");
        assert_eq!(
            RecordingsFile::open_kept(&dir, 1)
                .expect_err("a file cut short")
                .reason(),
            "too-short"
        );
        assert_eq!(std::fs::metadata(&path).expect("the file").len(), 40);
        std::fs::write(&path, vec![0u8; FILE_LENGTH]).expect("a file of zeros");
        assert_eq!(
            RecordingsFile::open_kept(&dir, 1)
                .expect_err("no magic")
                .reason(),
            "not-ready"
        );
        assert_eq!(
            std::fs::read(&path).expect("the file"),
            vec![0u8; FILE_LENGTH]
        );
        // The file a start created, and then a list: both open.
        drop(RecordingsFile::open(&dir, 1).expect("the file is created"));
        let mut file = RecordingsFile::open_kept(&dir, 1).expect("the created file opens");
        assert_eq!((file.entries().len(), file.floor_position()), (0, 0));
        file.write(0, [listed(0, 0)]).expect("the list is written");
        drop(file);
        let file = RecordingsFile::open_kept(&dir, 1).expect("the file opens");
        assert_eq!(file.entries(), &[listed(0, 0)]);
        assert_eq!(
            RecordingsFile::open_kept(&dir, 2)
                .expect_err("another member's")
                .reason(),
            "member"
        );
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    /// What a write of the list costs, for ADR 0108's Evidence: 500 writes of a list
    /// of 3 recordings and 500 of a full one, each synced, with the 50th and 99th
    /// percentile and the longest printed. Ignored, since it measures and asserts
    /// nothing: `cargo test -p cavalry-log --lib -- --ignored --nocapture
    /// a_write_of_the_list_timed`.
    #[test]
    #[ignore = "a measurement for ADR 0108, run by hand"]
    fn a_write_of_the_list_timed() {
        let dir = dir("timed");
        let mut file = RecordingsFile::open(&dir, 0).expect("the file is created");
        for count in [3usize, MAX_ENTRIES] {
            let list: Vec<ListedRecording> = (0..count as i64)
                .map(|index| listed(index, index * 32))
                .collect();
            let mut took = Vec::with_capacity(500);
            for _ in 0..500 {
                let from = std::time::Instant::now();
                file.write(0, list.iter().copied()).expect("a write");
                took.push(from.elapsed());
            }
            took.sort();
            println!(
                "{count} entries, 500 writes: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
                took[249].as_secs_f64() * 1e3,
                took[494].as_secs_f64() * 1e3,
                took[499].as_secs_f64() * 1e3
            );
        }
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_file_at_the_highest_sequence_takes_no_write() {
        let dir = dir("sequence");
        let path = RecordingsFile::path_in(&dir);
        drop(RecordingsFile::open(&dir, 3).expect("the file is created"));
        // No count of writes reaches this sequence: a file that holds it is damage,
        // and a write to it is refused where the sum would overflow.
        let mut slot = Box::new([0u8; SLOT_LENGTH]);
        let length = encode_slot(
            SlotFields {
                sequence: i64::MAX,
                floor_position: 0,
            },
            [listed(4, 0)],
            &mut slot,
        )
        .expect("a slot");
        let raw = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the file");
        raw.write_all_at(&slot[..length], slot_offset(1))
            .expect("a write");
        let before = std::fs::read(&path).expect("the file");
        let mut file = RecordingsFile::open(&dir, 3).expect("the file opens");
        assert_eq!(file.sequence(), i64::MAX);
        assert_eq!(
            file.write(0, [listed(4, 0), listed(5, 96)])
                .expect_err("no sequence above the highest")
                .reason(),
            "sequence"
        );
        assert_eq!(file.entries(), &[listed(4, 0)]);
        assert_eq!(std::fs::read(&path).expect("the file"), before);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_list_at_the_bound_is_written_and_one_past_it_is_refused() {
        let dir = dir("bound");
        let mut file = RecordingsFile::open(&dir, 0).expect("the file is created");
        let full: Vec<ListedRecording> = (0..MAX_ENTRIES as i64)
            .map(|index| listed(index, index * 32))
            .collect();
        file.write(0, full.iter().copied()).expect("a full list");
        let mut over = full.clone();
        over.push(listed(MAX_ENTRIES as i64, MAX_ENTRIES as i64 * 32));
        assert_eq!(
            file.write(0, over.iter().copied())
                .expect_err("one past the bound")
                .reason(),
            "log-recordings-full"
        );
        drop(file);
        let file = RecordingsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(file.entries(), &full[..]);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }
}
