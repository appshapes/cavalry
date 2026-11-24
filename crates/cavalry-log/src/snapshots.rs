//! The snapshots file: a cluster member's record of the snapshots in its log directory
//! that a start can begin from (`spec/durable-log.md`, version 4, "Snapshots"; ADR
//! 0128).
//!
//! A snapshot is a recording: the state of a member's consensus module, or of a
//! service beside the member, as it was at one position of the member's log
//! (`spec/cluster.md`, "Snapshots"). The catalog holds the recording, as it
//! holds every other, and does not say that it is a snapshot, of whom, or at which
//! position. It also holds a snapshot whose writer stopped half way. This file says
//! all of that: a snapshot is one a start may begin from once this file lists it,
//! and never before.
//!
//! The file lists at most [`MAX_ENTRIES`] snapshots, the newest, in the order they
//! were listed. Each entry names the recording, the position of the log the state
//! is of, the leadership term at that position with the term's base, the cluster's
//! time, and whose state it is: [`CONSENSUS_MODULE`] for the member's own, or a
//! service's id. One more snapshot than the file holds forgets the oldest, whose
//! recording stays in the catalog.
//!
//! The file has one writer, a member's consensus module, from its own thread:
//! [`SnapshotsFile::open`] at the member's start, which creates the file when there
//! is none, and [`SnapshotsFile::list`] for each snapshot, which syncs before it
//! returns. The list is written whole, into the slot of two that the list is not
//! read from, under a checksum and a sequence one above the other slot's, as the
//! log recordings file is ([`crate::recordings`]): a crash tears at most the slot
//! being written, whose checksum then fails, and the reader takes the other, the
//! list as it was. [`SnapshotsHeader::parse`], [`parse_slot`] and [`choose`] are the
//! checks of "What a reader rejects", in order, which the vectors of
//! `spec/vectors/durable-log.json` pin.
//!
//! Nothing here runs per message: the file is written once for each snapshot.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::layout::crc32;

/// The file's name in the log directory.
pub const FILE_NAME: &str = "snapshots";
/// The header's eight-byte magic, `CVLYSNAP`.
pub const MAGIC: [u8; 8] = *b"CVLYSNAP";
/// The one version this crate writes and reads.
pub const VERSION: i32 = 0;
/// The header's length in bytes.
pub const HEADER_LENGTH: usize = 64;
/// The most snapshots a list names, the newest.
pub const MAX_ENTRIES: usize = 64;
/// An entry's length in bytes.
pub const ENTRY_LENGTH: usize = 48;
/// The length of a slot's own fields, before its entries: the checksum, the entry
/// count and the sequence.
pub const SLOT_HEADER_LENGTH: usize = 16;
/// A slot's length in bytes: its fields and room for [`MAX_ENTRIES`] entries.
pub const SLOT_LENGTH: usize = SLOT_HEADER_LENGTH + MAX_ENTRIES * ENTRY_LENGTH;
/// The file's length in bytes, fixed: the header and two slots.
pub const FILE_LENGTH: usize = HEADER_LENGTH + 2 * SLOT_LENGTH;
/// The service id of a snapshot of the member's own state, its consensus module's.
/// In Aeron, this is the consensus module's `SERVICE_ID`.
pub const CONSENSUS_MODULE: i32 = -1;
/// A position's alignment: a frame's (`spec/log-buffer.md`, "Positions").
const ALIGNMENT: i64 = 32;

/// Why a snapshots file, or one of its slots, was refused: the checks of
/// `spec/durable-log.md`, "What a reader rejects", by name, and the I/O errors around
/// them.
#[derive(Debug)]
pub enum SnapshotsError {
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
    /// The magic is eight zero bytes: the creator has not finished.
    /// [`SnapshotsFile::open`] writes a torn creation again, and returns this only for
    /// a zero magic over a list, which creation never writes: damage.
    NotReady,
    /// The magic is not `CVLYSNAP`.
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
    /// [`SnapshotsError::Slots`].
    Checksum,
    /// Neither slot's checksum matches: damage, since a crash tears at most one slot.
    Slots,
    /// A slot whose checksum matches holds a sequence below 0, or both slots hold the
    /// same sequence: damage, since each write's sequence is one above the other
    /// slot's. [`SnapshotsFile::list`] returns it too, and writes nothing, for a file
    /// whose list holds the highest sequence an i64 has, which no count of writes
    /// reaches.
    Sequence {
        /// The sequence found.
        found: i64,
    },
    /// An entry's recording id is below 0.
    Recording {
        /// The recording id found.
        found: i64,
    },
    /// An entry's service id is below −1.
    Service {
        /// The service id found.
        found: i32,
    },
    /// An entry's leadership term id is below 0, or below the term of the entry
    /// before it that has its service id.
    Term {
        /// The leadership term id found.
        found: i64,
    },
    /// An entry's term base log position is below 0 or not a multiple of 32; or its
    /// log position is not a multiple of 32, is below its term base log position, or
    /// is not above the log position of the entry before it that has its service id.
    Position {
        /// The position found.
        found: i64,
    },
    /// A snapshot given to [`SnapshotsFile::list`] holds a value that a reader would
    /// refuse in the list; nothing was written, and the file is as it was. The fault
    /// is the writer's, where [`SnapshotsError::Recording`],
    /// [`SnapshotsError::Service`], [`SnapshotsError::Term`] and
    /// [`SnapshotsError::Position`] are a file's.
    List {
        /// The recording id, the service id, the term or the position the snapshot
        /// held.
        found: i64,
    },
}

impl SnapshotsError {
    /// The check's name as the spec and the vectors spell it.
    pub const fn reason(&self) -> &'static str {
        match self {
            SnapshotsError::Io { .. } => "io",
            SnapshotsError::TooShort { .. } => "too-short",
            SnapshotsError::Length { .. } => "length",
            SnapshotsError::NotReady => "not-ready",
            SnapshotsError::Magic => "magic",
            SnapshotsError::Version { .. } => "version",
            SnapshotsError::SlotLength { .. } => "slot-length",
            SnapshotsError::Member { .. } => "member",
            SnapshotsError::Checksum => "checksum",
            SnapshotsError::Slots => "slots",
            SnapshotsError::Sequence { .. } => "sequence",
            SnapshotsError::Recording { .. } => "recording",
            SnapshotsError::Service { .. } => "service",
            SnapshotsError::Term { .. } => "term",
            SnapshotsError::Position { .. } => "position",
            SnapshotsError::List { .. } => "list",
        }
    }
}

const DOC: &str = "docs/reference/durable-log.md#snapshots";

impl std::fmt::Display for SnapshotsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // What an operator does with a list that cannot be read: nothing to the
        // directory. The list is the only record of which recordings are snapshots a
        // start may begin from, and a file written by hand would name one that is not.
        let keep = "Leave this member stopped and keep its log directory as it is: the \
                    cluster goes on while a quorum of the others runs. Do not remove or \
                    edit the file: it is the only record of which snapshots this member \
                    may start from";
        match self {
            SnapshotsError::Io { what, path, source } => write!(
                f,
                "could not {what} the snapshots file {}: {source}. Check that the log \
                 directory exists, is writable by this user and has free space; see {DOC}",
                path.display()
            ),
            SnapshotsError::TooShort { found } => write!(
                f,
                "the snapshots file is {found} bytes, shorter than its {FILE_LENGTH}: the \
                 file is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Length { found } => write!(
                f,
                "the snapshots file is {found} bytes, longer than its {FILE_LENGTH}: the \
                 file is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::NotReady => write!(
                f,
                "the snapshots file has no magic over a list of snapshots, which its \
                 creation never leaves: the file is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Magic => write!(
                f,
                "the snapshots file does not begin with CVLYSNAP: it is not a snapshots \
                 file, or it is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Version { found } => write!(
                f,
                "the snapshots file is version {found}, and this version of Cavalry reads \
                 version {VERSION}: start the member with the version of Cavalry that \
                 wrote the directory; see {DOC}"
            ),
            SnapshotsError::SlotLength { found } => write!(
                f,
                "the snapshots file names a slot length of {found}, not {SLOT_LENGTH}: the \
                 file is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Member {
                found,
                expected: Some(expected),
            } => write!(
                f,
                "the log directory is member {found}'s, and this is member {expected}: \
                 start member {expected} with its own log directory, the one it was \
                 started with before; see {DOC}"
            ),
            SnapshotsError::Member {
                found,
                expected: None,
            } => write!(
                f,
                "the snapshots file names member {found}, which is no member id: the file \
                 is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Checksum => write!(
                f,
                "a slot of the snapshots file fails its checksum: the write a crash tore, \
                 which the other slot stands in for. Nothing is to be done; see {DOC}"
            ),
            SnapshotsError::Slots => write!(
                f,
                "neither slot of the snapshots file passes its checksum, and a crash tears \
                 one at most: the file is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Sequence { found } => write!(
                f,
                "a slot of the snapshots file holds the sequence {found}, which is below \
                 0, the other slot's own, or the highest there is, which no write reaches: \
                 the file is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Recording { found } => write!(
                f,
                "the snapshots file names the recording {found}, which is no recording \
                 id: the file is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Service { found } => write!(
                f,
                "the snapshots file names the service {found}, which is neither \
                 {CONSENSUS_MODULE}, the member's own state, nor a service's id: the file \
                 is damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Term { found } => write!(
                f,
                "the snapshots file holds the leadership term {found}, which is below 0 \
                 or below the term of an older snapshot of the same service: the file is \
                 damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::Position { found } => write!(
                f,
                "the snapshots file holds the position {found}, which is below 0, not a \
                 multiple of 32, a log position below its term's base, or a log position \
                 that is not above an older snapshot's of the same service: the file is \
                 damaged. {keep}; see {DOC}"
            ),
            SnapshotsError::List { found } => write!(
                f,
                "a snapshot given to the snapshots file holds {found}, which no snapshot \
                 in the list holds there; nothing was written and the file is as it was. \
                 This is a fault in Cavalry and not damage to the file: report it with \
                 the member's counters; see {DOC}"
            ),
        }
    }
}

impl std::error::Error for SnapshotsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SnapshotsError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// One snapshot, as the list names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListedSnapshot {
    /// The snapshot's recording in the catalog.
    pub recording_id: i64,
    /// The leadership term at [`log_position`](Self::log_position).
    pub leadership_term_id: i64,
    /// Where that term begins in the log.
    pub term_base_log_position: i64,
    /// The position of the log the state is of: every entry below it is in the
    /// state, and none at or above it.
    pub log_position: i64,
    /// The cluster's time when the snapshot was asked for, in milliseconds since
    /// 1970-01-01T00:00:00Z.
    pub timestamp: i64,
    /// Whose state it is: [`CONSENSUS_MODULE`] for the member's own, or a service's
    /// id, 0 or more.
    pub service_id: i32,
}

impl ListedSnapshot {
    fn encode(&self, out: &mut [u8]) {
        let fields = [
            self.recording_id,
            self.leadership_term_id,
            self.term_base_log_position,
            self.log_position,
            self.timestamp,
        ];
        for (index, field) in fields.iter().enumerate() {
            if let Some(bytes) = out.get_mut(index * 8..index * 8 + 8) {
                bytes.copy_from_slice(&field.to_le_bytes());
            }
        }
        if let Some(bytes) = out.get_mut(40..44) {
            bytes.copy_from_slice(&self.service_id.to_le_bytes());
        }
        // The reserved four bytes are written as 0.
        if let Some(bytes) = out.get_mut(44..ENTRY_LENGTH) {
            bytes.fill(0);
        }
    }

    fn decode(bytes: &[u8], at: usize) -> Self {
        Self {
            recording_id: i64_at(bytes, at),
            leadership_term_id: i64_at(bytes, at + 8),
            term_base_log_position: i64_at(bytes, at + 16),
            log_position: i64_at(bytes, at + 24),
            timestamp: i64_at(bytes, at + 32),
            service_id: i32_at(bytes, at + 40),
        }
    }
}

/// The file's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotsHeader {
    /// The member whose snapshots the file lists.
    pub member_id: i32,
}

impl SnapshotsHeader {
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
    pub fn parse(bytes: &[u8], file_length: u64) -> Result<Self, SnapshotsError> {
        if file_length < FILE_LENGTH as u64 {
            return Err(SnapshotsError::TooShort { found: file_length });
        }
        if file_length > FILE_LENGTH as u64 {
            return Err(SnapshotsError::Length { found: file_length });
        }
        let Some(header) = bytes.first_chunk::<HEADER_LENGTH>() else {
            return Err(SnapshotsError::TooShort {
                found: bytes.len() as u64,
            });
        };
        if header[..8] == [0u8; 8] {
            return Err(SnapshotsError::NotReady);
        }
        if header[..8] != MAGIC {
            return Err(SnapshotsError::Magic);
        }
        let version = i32_at(header, 8);
        if version != VERSION {
            return Err(SnapshotsError::Version { found: version });
        }
        let slot_length = i32_at(header, 12);
        if slot_length != SLOT_LENGTH as i32 {
            return Err(SnapshotsError::SlotLength { found: slot_length });
        }
        let member_id = i32_at(header, 16);
        if member_id < 0 {
            return Err(SnapshotsError::Member {
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

/// Writes a slot into `out`: the entry count, the sequence, the entries and the
/// checksum over them. Returns how many bytes the slot's fields and entries take,
/// which is what a write puts in the file; the rest of the slot is not read. More
/// than [`MAX_ENTRIES`] entries, or an entry a reader would refuse, is the reader's
/// refusal, `checksum` for the count, with nothing written.
pub fn encode_slot(
    sequence: i64,
    entries: &[ListedSnapshot],
    out: &mut [u8; SLOT_LENGTH],
) -> Result<usize, SnapshotsError> {
    if entries.len() > MAX_ENTRIES {
        return Err(SnapshotsError::Checksum);
    }
    for (index, entry) in entries.iter().enumerate() {
        check_entry(entry, entries.get(..index).unwrap_or(&[]))?;
    }
    for (index, entry) in entries.iter().enumerate() {
        let at = SLOT_HEADER_LENGTH + index * ENTRY_LENGTH;
        if let Some(bytes) = out.get_mut(at..at + ENTRY_LENGTH) {
            entry.encode(bytes);
        }
    }
    let count = entries.len();
    out[4..8].copy_from_slice(&(count as i32).to_le_bytes());
    out[8..16].copy_from_slice(&sequence.to_le_bytes());
    let checksum = slot_checksum(out, count);
    out[..4].copy_from_slice(&checksum.to_le_bytes());
    Ok(SLOT_HEADER_LENGTH + count * ENTRY_LENGTH)
}

/// The checksum of a slot that holds `count` entries: the zlib CRC32 of everything
/// after the checksum itself, the entry count, the sequence and the entries, bytes 4
/// to 16 + 48 × count.
fn slot_checksum(slot: &[u8], count: usize) -> u32 {
    crc32(
        slot.get(4..SLOT_HEADER_LENGTH + count * ENTRY_LENGTH)
            .unwrap_or(&[]),
    )
}

/// An entry's checks, in the order the spec gives them: `recording`, `service`,
/// `term`, `position`. `earlier` holds the entries before it in the list.
fn check_entry(entry: &ListedSnapshot, earlier: &[ListedSnapshot]) -> Result<(), SnapshotsError> {
    if entry.recording_id < 0 {
        return Err(SnapshotsError::Recording {
            found: entry.recording_id,
        });
    }
    if entry.service_id < CONSENSUS_MODULE {
        return Err(SnapshotsError::Service {
            found: entry.service_id,
        });
    }
    // The entry before it that has its service id: the newest older snapshot of the
    // same state.
    let before = earlier
        .iter()
        .rev()
        .find(|other| other.service_id == entry.service_id);
    let term = entry.leadership_term_id;
    if term < 0 || before.is_some_and(|other| term < other.leadership_term_id) {
        return Err(SnapshotsError::Term { found: term });
    }
    let base = entry.term_base_log_position;
    if base < 0 || base % ALIGNMENT != 0 {
        return Err(SnapshotsError::Position { found: base });
    }
    let position = entry.log_position;
    if position % ALIGNMENT != 0
        || position < base
        || before.is_some_and(|other| position <= other.log_position)
    {
        return Err(SnapshotsError::Position { found: position });
    }
    Ok(())
}

/// Reads a slot from `bytes`, which begin at the slot's first byte and hold at least
/// its fields and its entries; `entries` is emptied and takes the slot's entries.
/// Returns the slot's sequence. The checks are `checksum`, which an entry count
/// outside 0 to [`MAX_ENTRIES`] or bytes shorter than the count needs also fail, then
/// `sequence`, then `recording`, `service`, `term` and `position` for each entry in
/// order.
pub fn parse_slot(bytes: &[u8], entries: &mut Vec<ListedSnapshot>) -> Result<i64, SnapshotsError> {
    entries.clear();
    if bytes.len() < SLOT_HEADER_LENGTH {
        return Err(SnapshotsError::Checksum);
    }
    let count = i32_at(bytes, 4);
    let Some(count) = usize::try_from(count)
        .ok()
        .filter(|count| *count <= MAX_ENTRIES)
    else {
        return Err(SnapshotsError::Checksum);
    };
    if bytes.len() < SLOT_HEADER_LENGTH + count * ENTRY_LENGTH {
        return Err(SnapshotsError::Checksum);
    }
    let stored = u32::from_le_bytes(bytes.first_chunk::<4>().copied().unwrap_or_default());
    if stored != slot_checksum(bytes, count) {
        return Err(SnapshotsError::Checksum);
    }
    let sequence = i64_at(bytes, 8);
    if sequence < 0 {
        return Err(SnapshotsError::Sequence { found: sequence });
    }
    for index in 0..count {
        let entry = ListedSnapshot::decode(bytes, SLOT_HEADER_LENGTH + index * ENTRY_LENGTH);
        check_entry(&entry, entries)?;
        entries.push(entry);
    }
    Ok(sequence)
}

/// Which slot holds the list, from what [`parse_slot`] made of each: the one whose
/// checksum matches, and of two the one with the higher sequence. A slot whose
/// checksum fails is not read. Neither matching is `slots`; a slot whose checksum
/// matches and whose fields fail a check is that check's refusal, whatever the other
/// slot holds, and slot 0's when both do; two slots of one sequence are `sequence`.
pub fn choose(
    slot_0: Result<i64, SnapshotsError>,
    slot_1: Result<i64, SnapshotsError>,
) -> Result<usize, SnapshotsError> {
    match (slot_0, slot_1) {
        (Err(SnapshotsError::Checksum), Err(SnapshotsError::Checksum)) => {
            Err(SnapshotsError::Slots)
        }
        (Ok(_), Err(SnapshotsError::Checksum)) => Ok(0),
        (Err(SnapshotsError::Checksum), Ok(_)) => Ok(1),
        (Err(SnapshotsError::Checksum), Err(error)) | (Err(error), _) | (_, Err(error)) => {
            Err(error)
        }
        (Ok(first), Ok(second)) if first == second => {
            Err(SnapshotsError::Sequence { found: first })
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
pub struct SnapshotsFile {
    path: PathBuf,
    file: File,
    /// The slot the list is read from, and its sequence.
    slot: usize,
    sequence: i64,
    /// The list, at most [`MAX_ENTRIES`], allocated when the file is opened.
    entries: Vec<ListedSnapshot>,
    /// The list a write is building, allocated when the file is opened, so that a
    /// write allocates nothing.
    next: Vec<ListedSnapshot>,
    /// One slot's bytes, allocated when the file is opened, for the same reason.
    scratch: Box<[u8; SLOT_LENGTH]>,
    /// Whether this open wrote the file, new or over a torn creation.
    created: bool,
}

impl SnapshotsFile {
    /// The file's path in `dir`.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(FILE_NAME)
    }

    /// Opens member `member_id`'s file in `dir` and reads its list, and creates the
    /// file with an empty list when there is none: a log directory with no file
    /// lists no snapshot. A file a crash tore while it was created, shorter than its
    /// header or with a zero magic over no list, is written again; a zero magic over
    /// a list is [`SnapshotsError::NotReady`]. A file of another member's is
    /// [`SnapshotsError::Member`]. The file's length is checked before a byte is
    /// read, and what is read is its fixed length at most.
    pub fn open(dir: &Path, member_id: i32) -> Result<Self, SnapshotsError> {
        let path = Self::path_in(dir);
        let io = |what: &'static str| {
            let path = path.clone();
            move |source| SnapshotsError::Io { what, path, source }
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
            return Err(SnapshotsError::Length { found: file_length });
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
        // sequence 0, and no other. So under a zero magic a slot is the creation's
        // when its checksum fails, the slot no write reached, or when it holds that
        // list; any other slot whose checksum matches, a list a write made or one
        // that fails a later check, is damage and not a creation.
        let creations = |slot: &Result<i64, SnapshotsError>, list: &[ListedSnapshot]| match slot {
            Err(SnapshotsError::Checksum) => true,
            Ok(sequence) => *sequence == 0 && list.is_empty(),
            Err(_) => false,
        };
        let zero_magic = bytes.get(..8).is_some_and(|magic| magic == [0u8; 8]);
        let torn_creation = file_length < HEADER_LENGTH as u64
            || (zero_magic && creations(&slots[0], &lists[0]) && creations(&slots[1], &lists[1]));
        let mut scratch = Box::new([0u8; SLOT_LENGTH]);
        if torn_creation {
            Self::create(&file, dir, &path, member_id, &mut scratch)?;
            let [mut entries, mut next] = lists;
            entries.clear();
            next.clear();
            return Ok(Self {
                path,
                file,
                slot: 0,
                sequence: 0,
                entries,
                next,
                scratch,
                created: true,
            });
        }
        let header = SnapshotsHeader::parse(&bytes, file_length)?;
        if header.member_id != member_id {
            return Err(SnapshotsError::Member {
                found: header.member_id,
                expected: Some(member_id),
            });
        }
        let [slot_0, slot_1] = slots;
        let read = [slot_0.as_ref().ok().copied(), slot_1.as_ref().ok().copied()];
        let slot = choose(slot_0, slot_1)?;
        let [list_0, list_1] = lists;
        let (entries, mut next) = if slot == 0 {
            (list_0, list_1)
        } else {
            (list_1, list_0)
        };
        next.clear();
        Ok(Self {
            path,
            file,
            slot,
            sequence: read.get(slot).copied().flatten().unwrap_or(0),
            entries,
            next,
            scratch,
            created: false,
        })
    }

    /// Writes the created file over whatever `file` holds, the way the log recordings
    /// file is created: the file emptied, the header after its magic, slot 0 as the
    /// empty list of sequence 0 and the file's full length, synced; then the magic,
    /// synced; then the directory `dir`, synced, so that a power loss does not take
    /// the file's name from a directory in which the member has since listed a
    /// snapshot.
    fn create(
        file: &File,
        dir: &Path,
        path: &Path,
        member_id: i32,
        scratch: &mut [u8; SLOT_LENGTH],
    ) -> Result<(), SnapshotsError> {
        let io = |what: &'static str| {
            let path = path.to_path_buf();
            move |source| SnapshotsError::Io { what, path, source }
        };
        let header = SnapshotsHeader { member_id }.encode();
        let length = encode_slot(0, &[], scratch)?;
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
        crate::dir::sync_directory(dir).map_err(io("sync the directory of"))
    }

    /// Whether this open wrote the file: there was none, or a crash had torn the one
    /// a start before this one was creating. The list is then the one a creation
    /// writes, of no snapshot.
    pub const fn was_created(&self) -> bool {
        self.created
    }

    /// The list: the snapshots the member may start from, in the order they were
    /// listed, the newest last.
    pub fn entries(&self) -> &[ListedSnapshot] {
        &self.entries
    }

    /// The newest snapshot the list names for `service_id`: [`CONSENSUS_MODULE`] for
    /// the member's own state, or a service's id.
    pub fn newest(&self, service_id: i32) -> Option<&ListedSnapshot> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.service_id == service_id)
    }

    /// The sequence of the list the file holds: 0 for the list it was created with,
    /// and one more for each write.
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }

    /// Lists `snapshot` after the snapshots the file names, and syncs: one write of
    /// the slot the list is not read from, with the next sequence, and one sync. A
    /// crash before the sync has ended leaves the list as it was. A list that
    /// already names [`MAX_ENTRIES`] snapshots forgets one for the new one: the
    /// oldest whose service has a newer snapshot in the list, the new one counted,
    /// so that the newest snapshot of each service stays listed whatever the others
    /// are of (ADR 0133). The forgotten snapshot's recording stays in the catalog.
    /// A snapshot a reader
    /// would refuse in the list is [`SnapshotsError::List`], and nothing is written:
    /// a recording id below 0, a service id below −1, a term below 0, a position that
    /// is no position, or a term or a log position that does not carry on from the
    /// newest snapshot the list names for its service. Nor is anything written to a
    /// file that holds the highest sequence there is ([`SnapshotsError::Sequence`]).
    /// On an error the list in memory is the one before the call. The file's is too,
    /// but after a write that succeeded and a sync that failed: the slot then holds
    /// the new list, which a later open may read. That list names a snapshot that is
    /// whole, as every listed one is, and the next call writes the same slot again.
    pub fn list(&mut self, snapshot: ListedSnapshot) -> Result<(), SnapshotsError> {
        let Some(sequence) = self.sequence.checked_add(1) else {
            return Err(SnapshotsError::Sequence {
                found: self.sequence,
            });
        };
        // The list with the snapshot: every snapshot the file names and then the new
        // one. A full list leaves one out: the first whose service has a later
        // snapshot, in the list or as the new one, so that no service loses its
        // newest. Where every entry is the only one of its service and the new
        // snapshot is of yet another, there is none, and the first goes.
        let out = (self.entries.len() >= MAX_ENTRIES).then(|| {
            self.entries
                .iter()
                .enumerate()
                .position(|(index, entry)| {
                    entry.service_id == snapshot.service_id
                        || self
                            .entries
                            .iter()
                            .skip(index.saturating_add(1))
                            .any(|later| later.service_id == entry.service_id)
                })
                .unwrap_or(0)
        });
        self.next.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            if Some(index) != out {
                self.next.push(*entry);
            }
        }
        self.next.push(snapshot);
        // What a reader would call damage is, in a list not yet written, the
        // writer's fault: the file holds no such thing.
        let length =
            encode_slot(sequence, &self.next, &mut self.scratch).map_err(|error| match error {
                SnapshotsError::Recording { found }
                | SnapshotsError::Term { found }
                | SnapshotsError::Position { found } => SnapshotsError::List { found },
                SnapshotsError::Service { found } => SnapshotsError::List {
                    found: i64::from(found),
                },
                other => other,
            })?;
        let next = 1 - self.slot;
        let io = |what: &'static str, path: &Path| {
            let path = path.to_path_buf();
            move |source| SnapshotsError::Io { what, path, source }
        };
        self.file
            .write_all_at(self.scratch.get(..length).unwrap_or(&[]), slot_offset(next))
            .map_err(io("write", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        self.slot = next;
        self.sequence = sequence;
        std::mem::swap(&mut self.entries, &mut self.next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(test: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cavalry-log-snapshots-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a directory");
        path
    }

    /// A snapshot of `service_id` at `log_position`, in term 0, in recording
    /// `recording_id`.
    fn listed(recording_id: i64, service_id: i32, log_position: i64) -> ListedSnapshot {
        ListedSnapshot {
            recording_id,
            leadership_term_id: 0,
            term_base_log_position: 0,
            log_position,
            timestamp: 1_794_700_000_000 + log_position,
            service_id,
        }
    }

    #[test]
    fn a_full_list_forgets_its_oldest_snapshot_and_keeps_the_newest_of_each_service() {
        let dir = dir("full");
        let mut file = SnapshotsFile::open(&dir, 0).expect("the file is created");
        // The module and one service, a snapshot each at every action: 32 actions fill
        // the list.
        for action in 0..(MAX_ENTRIES as i64 / 2) {
            let position = (action + 1) * 320;
            file.list(listed(action * 2, CONSENSUS_MODULE, position))
                .expect("the module's snapshot is listed");
            file.list(listed(action * 2 + 1, 0, position))
                .expect("the service's snapshot is listed");
        }
        assert_eq!(file.entries().len(), MAX_ENTRIES);
        assert_eq!(
            file.entries().first(),
            Some(&listed(0, CONSENSUS_MODULE, 320))
        );
        // One more: the oldest goes, and the list is still at its bound.
        let newest = listed(64, CONSENSUS_MODULE, 33 * 320);
        file.list(newest).expect("one more is listed");
        assert_eq!(file.entries().len(), MAX_ENTRIES);
        assert_eq!(file.entries().first(), Some(&listed(1, 0, 320)));
        assert_eq!(file.entries().last(), Some(&newest));
        assert_eq!(file.newest(CONSENSUS_MODULE), Some(&newest));
        assert_eq!(file.newest(0), Some(&listed(63, 0, 32 * 320)));
        assert_eq!(file.newest(1), None);
        assert_eq!(file.sequence(), MAX_ENTRIES as i64 + 1);
        drop(file);
        let file = SnapshotsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(file.entries().len(), MAX_ENTRIES);
        assert_eq!(file.newest(CONSENSUS_MODULE), Some(&newest));
        assert_eq!(
            std::fs::metadata(SnapshotsFile::path_in(&dir))
                .expect("the file")
                .len(),
            FILE_LENGTH as u64
        );
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    /// A state whose snapshots stop keeps the one it has (ADR 0133). The service
    /// takes one snapshot and stops; the module goes on for more snapshots than the
    /// list holds. The service's one entry stays, with the module's newest 63, and it
    /// is the first of the list: each write left out the oldest of the module's,
    /// which had a newer one. The same file read back says the same.
    #[test]
    fn a_full_list_keeps_the_only_snapshot_of_a_service_whose_snapshots_stopped() {
        let dir = dir("stopped-service");
        let mut file = SnapshotsFile::open(&dir, 0).expect("the file is created");
        let service = listed(0, 0, 320);
        file.list(service)
            .expect("the service's snapshot is listed");
        let modules = MAX_ENTRIES as i64 + 36;
        for action in 1..=modules {
            file.list(listed(action, CONSENSUS_MODULE, action * 320))
                .expect("the module's snapshot is listed");
        }
        let check = |file: &SnapshotsFile| {
            assert_eq!(file.entries().len(), MAX_ENTRIES);
            assert_eq!(file.entries().first(), Some(&service));
            assert_eq!(file.newest(0), Some(&service));
            assert_eq!(
                file.newest(CONSENSUS_MODULE),
                Some(&listed(modules, CONSENSUS_MODULE, modules * 320))
            );
            // The module's entries are its newest 63, in the order they were listed.
            let first_kept = modules - (MAX_ENTRIES as i64 - 1) + 1;
            for (entry, action) in file.entries().iter().skip(1).zip(first_kept..) {
                assert_eq!(entry, &listed(action, CONSENSUS_MODULE, action * 320));
            }
        };
        check(&file);
        // The service comes back and takes a snapshot: its older one has a newer one
        // now, and is the one left out.
        let again = listed(modules + 1, 0, (modules + 1) * 320);
        file.list(again).expect("the service's snapshot is listed");
        assert_eq!(file.entries().len(), MAX_ENTRIES);
        assert_eq!(file.newest(0), Some(&again));
        assert!(!file.entries().contains(&service));
        drop(file);
        let file = SnapshotsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(file.newest(0), Some(&again));
        assert_eq!(file.entries().len(), MAX_ENTRIES);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    /// The bound of the rule: 64 entries of 64 services, and a snapshot of a
    /// sixty-fifth. No entry's service has a later one, so the first entry is left
    /// out, as the page says.
    #[test]
    fn a_full_list_of_one_snapshot_for_each_service_forgets_its_first() {
        let dir = dir("every-service");
        let mut file = SnapshotsFile::open(&dir, 0).expect("the file is created");
        for service in 0..MAX_ENTRIES as i32 {
            file.list(listed(i64::from(service), service, 320))
                .expect("a service's snapshot is listed");
        }
        assert_eq!(file.entries().len(), MAX_ENTRIES);
        let one_more = listed(MAX_ENTRIES as i64, MAX_ENTRIES as i32, 320);
        file.list(one_more).expect("one more is listed");
        assert_eq!(file.entries().len(), MAX_ENTRIES);
        assert_eq!(file.newest(0), None);
        assert_eq!(file.entries().first(), Some(&listed(1, 1, 320)));
        assert_eq!(file.entries().last(), Some(&one_more));
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_new_file_lists_no_snapshot_and_a_listed_snapshot_is_read_back() {
        let dir = dir("round-trip");
        let mut file = SnapshotsFile::open(&dir, 2).expect("the file is created");
        assert!(file.was_created());
        assert_eq!(file.entries(), &[]);
        assert_eq!(file.sequence(), 0);
        assert_eq!(file.newest(CONSENSUS_MODULE), None);
        assert_eq!(
            std::fs::metadata(SnapshotsFile::path_in(&dir))
                .expect("the file")
                .len(),
            FILE_LENGTH as u64
        );
        let module = ListedSnapshot {
            recording_id: 7,
            leadership_term_id: 3,
            term_base_log_position: 4096,
            log_position: 8192,
            timestamp: 1_794_700_000_123,
            service_id: CONSENSUS_MODULE,
        };
        let service = ListedSnapshot {
            recording_id: 8,
            service_id: 0,
            ..module
        };
        file.list(module).expect("the module's snapshot is listed");
        file.list(service)
            .expect("the service's snapshot is listed");
        assert_eq!(file.entries(), &[module, service]);
        assert_eq!(file.sequence(), 2);
        drop(file);
        let file = SnapshotsFile::open(&dir, 2).expect("the file opens");
        assert!(!file.was_created());
        assert_eq!(file.entries(), &[module, service]);
        assert_eq!(file.newest(CONSENSUS_MODULE), Some(&module));
        assert_eq!(file.newest(0), Some(&service));
        assert_eq!(file.sequence(), 2);
        // Another member's directory is refused, with both ids.
        let refused = SnapshotsFile::open(&dir, 1).expect_err("another member's");
        assert_eq!(refused.reason(), "member");
        assert!(refused.to_string().contains("member 2's"), "{refused}");
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_snapshot_a_reader_would_refuse_is_not_listed_and_the_list_is_as_it_was() {
        let dir = dir("refused");
        let mut file = SnapshotsFile::open(&dir, 0).expect("the file is created");
        let first = ListedSnapshot {
            recording_id: 4,
            leadership_term_id: 2,
            term_base_log_position: 640,
            log_position: 960,
            timestamp: 5,
            service_id: CONSENSUS_MODULE,
        };
        file.list(first).expect("the snapshot is listed");
        let before = std::fs::read(SnapshotsFile::path_in(&dir)).expect("the file");
        for (what, snapshot) in [
            (
                "a recording id below 0",
                ListedSnapshot {
                    recording_id: -1,
                    log_position: 1280,
                    ..first
                },
            ),
            (
                "a service id below -1",
                ListedSnapshot {
                    service_id: -2,
                    ..first
                },
            ),
            (
                "a term below 0",
                ListedSnapshot {
                    leadership_term_id: -1,
                    service_id: 0,
                    ..first
                },
            ),
            (
                "a term below the module's last",
                ListedSnapshot {
                    leadership_term_id: 1,
                    log_position: 1280,
                    ..first
                },
            ),
            (
                "a term base that is not aligned",
                ListedSnapshot {
                    term_base_log_position: 650,
                    log_position: 1280,
                    ..first
                },
            ),
            (
                "a log position below its term's base",
                ListedSnapshot {
                    log_position: 608,
                    service_id: 0,
                    ..first
                },
            ),
            (
                "a log position that is not aligned",
                ListedSnapshot {
                    log_position: 1290,
                    ..first
                },
            ),
            ("the module's position again", first),
            (
                "a position below the module's last",
                ListedSnapshot {
                    log_position: 928,
                    ..first
                },
            ),
        ] {
            let refused = file.list(snapshot).expect_err(what);
            assert_eq!(refused.reason(), "list", "{what}");
            let text = refused.to_string();
            assert!(text.contains("a fault in Cavalry"), "{what}: {text}");
            assert!(text.contains(DOC), "{what}: {text}");
        }
        assert_eq!(file.entries(), &[first]);
        assert_eq!(file.sequence(), 1);
        assert_eq!(
            std::fs::read(SnapshotsFile::path_in(&dir)).expect("the file"),
            before
        );
        // A service's snapshot at the module's position is another state's, and is
        // listed; so is the module's next, in a later term at a higher position.
        let service = ListedSnapshot {
            recording_id: 5,
            service_id: 0,
            ..first
        };
        let later = ListedSnapshot {
            recording_id: 6,
            leadership_term_id: 3,
            term_base_log_position: 1280,
            log_position: 1280,
            ..first
        };
        file.list(service)
            .expect("a service's at the same position");
        file.list(later).expect("the module's next");
        assert_eq!(file.entries(), &[first, service, later]);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_torn_write_reads_as_the_list_before_it() {
        let dir = dir("torn-write");
        let mut file = SnapshotsFile::open(&dir, 0).expect("the file is created");
        file.list(listed(0, CONSENSUS_MODULE, 320))
            .expect("the snapshot is listed");
        file.list(listed(1, 0, 320))
            .expect("the snapshot is listed");
        // Sequence 2 is in slot 0, sequence 1 in slot 1. A crash tore the write of
        // sequence 3 into slot 1: its count and sequence are there, its entries not.
        drop(file);
        let path = SnapshotsFile::path_in(&dir);
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
        let mut file = SnapshotsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(
            file.entries(),
            &[listed(0, CONSENSUS_MODULE, 320), listed(1, 0, 320)]
        );
        assert_eq!(file.sequence(), 2);
        // The next write goes over the torn slot.
        file.list(listed(2, CONSENSUS_MODULE, 640))
            .expect("the snapshot is listed");
        drop(file);
        let file = SnapshotsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(file.entries().len(), 3);
        assert_eq!(file.sequence(), 3);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_torn_creation_is_written_again_and_damage_is_refused() {
        let dir = dir("damage");
        let path = SnapshotsFile::path_in(&dir);
        // A file shorter than its header is a creation the crash took inside its
        // first write, whatever it holds. It is created again, at its full length,
        // and is whole from then on.
        let mut cut = SnapshotsHeader { member_id: 1 }.encode();
        cut[..8].fill(0);
        for short in [&cut[..40], &[0xFFu8; HEADER_LENGTH - 1][..]] {
            std::fs::write(&path, short).expect("a file cut short of its header");
            let file = SnapshotsFile::open(&dir, 1).expect("the file is created again");
            assert_eq!(file.entries(), &[]);
            assert!(file.was_created());
            drop(file);
            assert_eq!(
                std::fs::metadata(&path).expect("the file").len(),
                FILE_LENGTH as u64
            );
            assert!(!SnapshotsFile::open(&dir, 1).expect("whole").was_created());
        }
        // A creation the crash took before its magic: the file is created again.
        std::fs::write(&path, vec![0u8; FILE_LENGTH]).expect("a file of zeros");
        let file = SnapshotsFile::open(&dir, 1).expect("the file is created again");
        assert!(file.was_created());
        drop(file);
        let raw = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the file");
        // A creation the crash took between its two syncs: everything but the magic,
        // with the list a creation writes in slot 0. Created again.
        raw.write_all_at(&[0u8; 8], 0).expect("a write");
        let file = SnapshotsFile::open(&dir, 1).expect("the file is created again");
        assert!(file.was_created());
        drop(file);
        // A zero magic over a slot whose checksum matches and which is not that list
        // is damage, though the slot fails a later check: here a sequence of −1.
        // Refused, and left as it was.
        let mut slot = [0u8; SLOT_HEADER_LENGTH];
        slot[8..16].copy_from_slice(&(-1i64).to_le_bytes());
        let checksum = crc32(&slot[4..]);
        slot[..4].copy_from_slice(&checksum.to_le_bytes());
        raw.write_all_at(&[0u8; 8], 0).expect("a write");
        raw.write_all_at(&slot, slot_offset(0)).expect("a write");
        let before = std::fs::read(&path).expect("the file");
        assert_eq!(
            SnapshotsFile::open(&dir, 1)
                .expect_err("a zero magic over a slot no creation writes")
                .reason(),
            "not-ready"
        );
        assert_eq!(std::fs::read(&path).expect("the file"), before);
        // The same slot under a magic is its own check's refusal.
        raw.write_all_at(&MAGIC, 0).expect("a write");
        assert_eq!(
            SnapshotsFile::open(&dir, 1)
                .expect_err("a sequence below 0")
                .reason(),
            "sequence"
        );
        drop(raw);
        std::fs::remove_file(&path).expect("the file is removed");
        let mut file = SnapshotsFile::open(&dir, 1).expect("the file is created");
        file.list(listed(5, CONSENSUS_MODULE, 320))
            .expect("the snapshot is listed");
        drop(file);
        // A zero magic over a list is damage: creation never writes one.
        let raw = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the file");
        raw.write_all_at(&[0u8; 8], 0).expect("a write");
        let refused = SnapshotsFile::open(&dir, 1).expect_err("a zero magic over a list");
        assert_eq!(refused.reason(), "not-ready");
        let text = refused.to_string();
        assert!(text.contains("Leave this member stopped"), "{text}");
        assert!(text.contains(DOC), "{text}");
        raw.write_all_at(&MAGIC, 0).expect("a write");
        assert_eq!(
            SnapshotsFile::open(&dir, 1).expect("whole").entries().len(),
            1
        );
        // Both slots torn: damage.
        raw.write_all_at(&[0xFF; 4], slot_offset(0))
            .expect("a write");
        raw.write_all_at(&[0xFF; 4], slot_offset(1))
            .expect("a write");
        assert_eq!(
            SnapshotsFile::open(&dir, 1)
                .expect_err("neither slot whole")
                .reason(),
            "slots"
        );
        // A file cut short, and one with bytes after its end.
        raw.set_len(FILE_LENGTH as u64 - 1).expect("a length");
        assert_eq!(
            SnapshotsFile::open(&dir, 1)
                .expect_err("cut short")
                .reason(),
            "too-short"
        );
        raw.set_len(FILE_LENGTH as u64 + 1).expect("a length");
        assert_eq!(
            SnapshotsFile::open(&dir, 1).expect_err("too long").reason(),
            "length"
        );
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn a_file_at_the_highest_sequence_takes_no_write() {
        let dir = dir("sequence");
        drop(SnapshotsFile::open(&dir, 0).expect("the file is created"));
        // The list of the highest sequence there is, written by hand into slot 1.
        let mut slot = Box::new([0u8; SLOT_LENGTH]);
        let length =
            encode_slot(i64::MAX, &[listed(0, CONSENSUS_MODULE, 320)], &mut slot).expect("a slot");
        let path = SnapshotsFile::path_in(&dir);
        let raw = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the file");
        raw.write_all_at(&slot[..length], slot_offset(1))
            .expect("a write");
        drop(raw);
        let before = std::fs::read(&path).expect("the file");
        let mut file = SnapshotsFile::open(&dir, 0).expect("the file opens");
        assert_eq!(file.sequence(), i64::MAX);
        let refused = file
            .list(listed(1, CONSENSUS_MODULE, 640))
            .expect_err("no sequence above the highest");
        assert_eq!(refused.reason(), "sequence");
        assert_eq!(file.entries(), &[listed(0, CONSENSUS_MODULE, 320)]);
        assert_eq!(std::fs::read(&path).expect("the file"), before);
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    /// What listing a snapshot costs its writer: one write of a slot and one sync,
    /// timed over 500 listings into a list of few snapshots and 500 into a full one,
    /// the median, the 99th percentile and the longest printed. Ignored, since it
    /// measures and asserts nothing: `cargo test -p cavalry-log --lib -- --ignored
    /// --nocapture a_listing_timed`.
    #[test]
    #[ignore = "a measurement for ADR 0128, run by hand"]
    fn a_listing_timed() {
        let dir = dir("timed");
        let mut file = SnapshotsFile::open(&dir, 0).expect("the file is created");
        let mut position = 0;
        for (what, full) in [("a list of at most 3", false), ("a full list", true)] {
            while full && file.entries().len() < MAX_ENTRIES {
                position += 320;
                file.list(listed(position / 320, CONSENSUS_MODULE, position))
                    .expect("a listing");
            }
            let mut took = Vec::with_capacity(500);
            for round in 0..500 {
                // A new file for every third listing keeps the short list short.
                if !full && round % 3 == 0 {
                    drop(file);
                    std::fs::remove_file(SnapshotsFile::path_in(&dir)).expect("removed");
                    file = SnapshotsFile::open(&dir, 0).expect("the file is created");
                }
                position += 320;
                let snapshot = listed(position / 320, CONSENSUS_MODULE, position);
                let from = std::time::Instant::now();
                file.list(snapshot).expect("a listing");
                took.push(from.elapsed());
            }
            took.sort();
            println!(
                "{what}, 500 listings: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
                took[249].as_secs_f64() * 1e3,
                took[494].as_secs_f64() * 1e3,
                took[499].as_secs_f64() * 1e3
            );
        }
        std::fs::remove_dir_all(&dir).expect("the directory is removed");
    }

    #[test]
    fn the_lengths_are_the_spec_s() {
        assert_eq!(SLOT_LENGTH, 3088);
        assert_eq!(FILE_LENGTH, 6240);
    }
}
