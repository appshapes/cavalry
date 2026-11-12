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
//! The consensus module of `cavalry-raft` is the file's only writer, from its own
//! thread: [`RecordingsFile::open`] at the member's start, which creates the file when
//! there is none, and [`RecordingsFile::write`] each time the list changes, which
//! syncs before it returns. The list is written whole, into the slot of two that the
//! list is not read from, under a checksum and a sequence one above the other slot's:
//! a crash tears at most the slot being written, whose checksum then fails, and the
//! reader takes the other, the list as it was. [`RecordingsHeader::parse`],
//! [`parse_slot`] and [`choose`] are the checks of "What a reader rejects", in order,
//! which the vectors of `spec/vectors/durable-log.json` pin.
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
/// count and the sequence.
pub const SLOT_HEADER_LENGTH: usize = 16;
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
    /// The magic is eight zero bytes: the creator has not finished. The writer writes
    /// a torn creation again, so [`RecordingsFile::open`] returns this only for a zero
    /// magic over a list, which creation never writes: damage.
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
    /// slot's.
    Sequence {
        /// The sequence found.
        found: i64,
    },
    /// An entry's recording id is below 0.
    Recording {
        /// The recording id found.
        found: i64,
    },
    /// An entry's start position is below 0, not a multiple of 32, or below the start
    /// position of the entry before it.
    Position {
        /// The start position found.
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
            RecordingsError::Full => "recordings-full",
        }
    }
}

const DOC: &str = "docs/reference/durable-log.md#log-recordings";

impl std::fmt::Display for RecordingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let damaged = "the file is damaged, and the member's log cannot be told from the \
                       other recordings in the directory: start this member on a new log \
                       directory, and it replicates the log from its leader";
        match self {
            RecordingsError::Io { what, path, source } => write!(
                f,
                "could not {what} the log recordings file {}: {source}. Check that the \
                 log directory exists, is writable by this user and has free space; see \
                 {DOC}",
                path.display()
            ),
            RecordingsError::TooShort { found } => write!(
                f,
                "the log recordings file is {found} bytes, shorter than its {FILE_LENGTH}: \
                 {damaged}; see {DOC}"
            ),
            RecordingsError::Length { found } => write!(
                f,
                "the log recordings file is {found} bytes, longer than its {FILE_LENGTH}: \
                 {damaged}; see {DOC}"
            ),
            RecordingsError::NotReady => write!(
                f,
                "the log recordings file has no magic over a list of recordings, which \
                 its creation never leaves: {damaged}; see {DOC}"
            ),
            RecordingsError::Magic => write!(
                f,
                "the log recordings file does not begin with CVLYLOGR: it is not a log \
                 recordings file, or {damaged}; see {DOC}"
            ),
            RecordingsError::Version { found } => write!(
                f,
                "the log recordings file is version {found}, and this version reads \
                 version {VERSION}: run the version of Cavalry that wrote the directory, \
                 or start this member on a new log directory; see {DOC}"
            ),
            RecordingsError::SlotLength { found } => write!(
                f,
                "the log recordings file names a slot length of {found}, not \
                 {SLOT_LENGTH}: {damaged}; see {DOC}"
            ),
            RecordingsError::Member {
                found,
                expected: Some(expected),
            } => write!(
                f,
                "the log directory is member {found}'s, and this is member {expected}: \
                 start member {expected} on its own log directory, the one given to it \
                 before, or on a new one; see {DOC}"
            ),
            RecordingsError::Member {
                found,
                expected: None,
            } => write!(
                f,
                "the log recordings file names member {found}, which is no member id: \
                 {damaged}; see {DOC}"
            ),
            RecordingsError::Checksum => write!(
                f,
                "a slot of the log recordings file fails its checksum: the write a crash \
                 tore, which the other slot stands in for; see {DOC}"
            ),
            RecordingsError::Slots => write!(
                f,
                "neither slot of the log recordings file passes its checksum, and a \
                 crash tears one at most: {damaged}; see {DOC}"
            ),
            RecordingsError::Sequence { found } => write!(
                f,
                "a slot of the log recordings file holds the sequence {found}, below 0 \
                 or the other slot's own: {damaged}; see {DOC}"
            ),
            RecordingsError::Recording { found } => write!(
                f,
                "the log recordings file names the recording {found}, which is no \
                 recording id: {damaged}; see {DOC}"
            ),
            RecordingsError::Position { found } => write!(
                f,
                "the log recordings file names a recording that begins at {found}, \
                 which is below 0, not a multiple of 32 or below the recording before \
                 it: {damaged}; see {DOC}"
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

/// Writes a slot into `out`: the sequence, the entries and the checksum over both.
/// Returns how many bytes the slot's fields and entries take, which is what a write
/// puts in the file; the rest of the slot is not read. More than [`MAX_ENTRIES`]
/// entries are [`RecordingsError::Full`], and an entry a reader would refuse is the
/// reader's refusal, with nothing written.
pub fn encode_slot(
    sequence: i64,
    entries: impl IntoIterator<Item = ListedRecording>,
    out: &mut [u8; SLOT_LENGTH],
) -> Result<usize, RecordingsError> {
    let mut count = 0usize;
    let mut previous = 0i64;
    for entry in entries {
        if count == MAX_ENTRIES {
            return Err(RecordingsError::Full);
        }
        check_entry(&entry, previous)?;
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
    out[8..16].copy_from_slice(&sequence.to_le_bytes());
    let checksum = slot_checksum(out, count);
    out[..4].copy_from_slice(&checksum.to_le_bytes());
    Ok(length)
}

/// The checksum of a slot that holds `count` entries: the zlib CRC32 of everything
/// after the checksum itself, the entry count, the sequence and the entries, bytes 4
/// to 16 + 16 × count.
fn slot_checksum(slot: &[u8], count: usize) -> u32 {
    crc32(
        slot.get(4..SLOT_HEADER_LENGTH + count * ENTRY_LENGTH)
            .unwrap_or(&[]),
    )
}

fn check_entry(entry: &ListedRecording, previous: i64) -> Result<(), RecordingsError> {
    if entry.recording_id < 0 {
        return Err(RecordingsError::Recording {
            found: entry.recording_id,
        });
    }
    let start = entry.start_position;
    if start < 0 || start % ALIGNMENT != 0 || start < previous {
        return Err(RecordingsError::Position { found: start });
    }
    Ok(())
}

/// Reads a slot from `bytes`, which begin at the slot's first byte and hold at least
/// its fields and its entries; `entries` is emptied and takes the slot's entries.
/// Returns the slot's sequence. The checks are `checksum`, which an entry count
/// outside 0 to [`MAX_ENTRIES`] or bytes shorter than the count needs also fail,
/// then `sequence`, then `recording` and `position` for each entry in order.
pub fn parse_slot(
    bytes: &[u8],
    entries: &mut Vec<ListedRecording>,
) -> Result<i64, RecordingsError> {
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
    let mut previous = 0i64;
    for index in 0..count {
        let at = SLOT_HEADER_LENGTH + index * ENTRY_LENGTH;
        let entry = ListedRecording {
            recording_id: i64_at(bytes, at),
            start_position: i64_at(bytes, at + 8),
        };
        check_entry(&entry, previous)?;
        previous = entry.start_position;
        entries.push(entry);
    }
    Ok(sequence)
}

/// Which slot holds the list, from what [`parse_slot`] made of each: the one whose
/// checksum matches, and of two the one with the higher sequence. A slot whose
/// checksum fails is not read. Neither matching is `slots`; a slot whose checksum
/// matches and whose fields fail a check is that check's refusal; two slots of one
/// sequence are `sequence`.
pub fn choose(
    slot_0: Result<i64, RecordingsError>,
    slot_1: Result<i64, RecordingsError>,
) -> Result<usize, RecordingsError> {
    match (slot_0, slot_1) {
        (Err(RecordingsError::Checksum), Err(RecordingsError::Checksum)) => {
            Err(RecordingsError::Slots)
        }
        (Ok(_), Err(RecordingsError::Checksum)) => Ok(0),
        (Err(RecordingsError::Checksum), Ok(_)) => Ok(1),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(first), Ok(second)) if first == second => {
            Err(RecordingsError::Sequence { found: first })
        }
        (Ok(first), Ok(second)) => Ok(usize::from(second > first)),
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
    /// The slot the list is read from, and its sequence.
    slot: usize,
    sequence: i64,
    /// The list, at most [`MAX_ENTRIES`], allocated when the file is opened.
    entries: Vec<ListedRecording>,
    /// One slot's bytes, allocated when the file is opened, so that a write
    /// allocates nothing.
    scratch: Box<[u8; SLOT_LENGTH]>,
}

impl RecordingsFile {
    /// The file's path in `dir`.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(FILE_NAME)
    }

    /// Opens member `member_id`'s file in `dir` and reads its list, and creates the
    /// file with an empty list when there is none. A file a crash tore while it was
    /// created, shorter than its header or with a zero magic over no list, is written
    /// again; a zero magic over a list is [`RecordingsError::NotReady`]. A file of
    /// another member's is [`RecordingsError::Member`]. The file's length is checked
    /// before a byte is read, and what is read is its fixed length at most.
    pub fn open(dir: &Path, member_id: i32) -> Result<Self, RecordingsError> {
        let path = Self::path_in(dir);
        let io = |what: &'static str| {
            let path = path.clone();
            move |source| RecordingsError::Io { what, path, source }
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io("open"))?;
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
        // sequence 0, so a zero magic over any other list is not one.
        let written = |slot: &Result<i64, RecordingsError>, list: &Vec<ListedRecording>| matches!(slot, Ok(sequence) if *sequence > 0 || !list.is_empty());
        let zero_magic = bytes.get(..8).is_some_and(|magic| magic == [0u8; 8]);
        let torn_creation = file_length < HEADER_LENGTH as u64
            || (zero_magic && !written(&slots[0], &lists[0]) && !written(&slots[1], &lists[1]));
        let mut scratch = Box::new([0u8; SLOT_LENGTH]);
        if torn_creation {
            Self::create(&file, &path, member_id, &mut scratch)?;
            let [mut entries, _] = lists;
            entries.clear();
            return Ok(Self {
                path,
                file,
                slot: 0,
                sequence: 0,
                entries,
                scratch,
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
        let sequences = [slot_0.as_ref().ok().copied(), slot_1.as_ref().ok().copied()];
        let slot = choose(slot_0, slot_1)?;
        let [list_0, list_1] = lists;
        Ok(Self {
            path,
            file,
            slot,
            sequence: sequences.get(slot).copied().flatten().unwrap_or(0),
            entries: if slot == 0 { list_0 } else { list_1 },
            scratch,
        })
    }

    /// Writes the created file over whatever `file` holds, the way the terms file is
    /// created: the file emptied, the header after its magic, slot 0 as the empty list
    /// of sequence 0 and the file's full length, synced; then the magic, synced.
    fn create(
        file: &File,
        path: &Path,
        member_id: i32,
        scratch: &mut [u8; SLOT_LENGTH],
    ) -> Result<(), RecordingsError> {
        let io = |what: &'static str| {
            let path = path.to_path_buf();
            move |source| RecordingsError::Io { what, path, source }
        };
        let header = RecordingsHeader { member_id }.encode();
        let length = encode_slot(0, [], scratch)?;
        file.set_len(0).map_err(io("empty"))?;
        file.write_all_at(&header[8..], 8)
            .map_err(io("write the header of"))?;
        file.write_all_at(scratch.get(..length).unwrap_or(&[]), slot_offset(0))
            .map_err(io("write the first list of"))?;
        file.set_len(FILE_LENGTH as u64).map_err(io("extend"))?;
        file.sync_all().map_err(io("sync"))?;
        file.write_all_at(&header[..8], 0)
            .map_err(io("write the magic of"))?;
        file.sync_all().map_err(io("sync"))
    }

    /// The list: the recordings that hold the member's log, in the order they carry
    /// it on.
    pub fn entries(&self) -> &[ListedRecording] {
        &self.entries
    }

    /// The sequence of the list the file holds: 0 for the list it was created with,
    /// and one more for each write.
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }

    /// Replaces the list with `entries` and syncs: one write of the slot the list is
    /// not read from, with the next sequence, and one sync. A crash before the sync
    /// has ended leaves the list as it was. A list a reader would refuse, or one of
    /// more than [`MAX_ENTRIES`], is refused and nothing is written. On an error the
    /// list in memory is the one before the call, as the file's is.
    pub fn write(
        &mut self,
        entries: impl IntoIterator<Item = ListedRecording> + Clone,
    ) -> Result<(), RecordingsError> {
        let sequence = self.sequence + 1;
        let length = encode_slot(sequence, entries.clone(), &mut self.scratch)?;
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
        self.sequence = sequence;
        self.entries.clear();
        self.entries.extend(entries);
        Ok(())
    }
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
        assert_eq!(
            std::fs::metadata(RecordingsFile::path_in(&dir))
                .expect("the file")
                .len(),
            FILE_LENGTH as u64
        );
        let first = [listed(0, 0)];
        file.write(first).expect("the list is written");
        let second = [listed(0, 0), listed(3, 192), listed(4, 160)];
        // An entry that begins below the one before it is refused, and nothing changes.
        assert_eq!(
            file.write(second)
                .expect_err("an entry out of order")
                .reason(),
            "position"
        );
        assert_eq!(file.entries(), &first);
        let second = [listed(0, 0), listed(3, 160), listed(4, 192)];
        file.write(second).expect("the list is written");
        assert_eq!(file.entries(), &second);
        assert_eq!(file.sequence(), 2);
        drop(file);
        let file = RecordingsFile::open(&dir, 2).expect("the file opens");
        assert_eq!(file.entries(), &second);
        assert_eq!(file.sequence(), 2);
        // Another member's directory is refused, with both ids.
        let refused = RecordingsFile::open(&dir, 1).expect_err("another member's");
        assert_eq!(refused.reason(), "member");
        assert!(refused.to_string().contains("member 2's"), "{refused}");
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_torn_write_reads_as_the_list_before_it() {
        let dir = dir("torn-write");
        let mut file = RecordingsFile::open(&dir, 0).expect("the file is created");
        file.write([listed(0, 0)]).expect("the list is written");
        file.write([listed(0, 0), listed(1, 96)])
            .expect("the list is written");
        // Sequence 2 is in slot 0, sequence 1 in slot 1. A crash tore the write of
        // sequence 3 into slot 1: its sequence and count are there, its entries not.
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
        file.write([listed(0, 0), listed(1, 96), listed(2, 96)])
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
        let mut file = RecordingsFile::open(&dir, 1).expect("the file is created again");
        assert_eq!(file.entries(), &[]);
        file.write([listed(5, 0)]).expect("the list is written");
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
    fn a_list_at_the_bound_is_written_and_one_past_it_is_refused() {
        let dir = dir("bound");
        let mut file = RecordingsFile::open(&dir, 0).expect("the file is created");
        let full: Vec<ListedRecording> = (0..MAX_ENTRIES as i64)
            .map(|index| listed(index, index * 32))
            .collect();
        file.write(full.iter().copied()).expect("a full list");
        let mut over = full.clone();
        over.push(listed(MAX_ENTRIES as i64, MAX_ENTRIES as i64 * 32));
        assert_eq!(
            file.write(over.iter().copied())
                .expect_err("one past the bound")
                .reason(),
            "recordings-full"
        );
        drop(file);
        let file = RecordingsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(file.entries(), &full[..]);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }
}
