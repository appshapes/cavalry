//! The terms file: a cluster member's record of the terms it has recorded and the
//! candidate term it has stored (`spec/durable-log.md`, version 2, "Terms"; ADR 0060).
//!
//! The consensus module of `cavalry-raft` is the file's only writer, from its own
//! thread: [`TermsFile::create`] or [`TermsFile::open`] at the member's start, then
//! [`TermsFile::append`] before the member reports an append position for a term,
//! [`TermsFile::truncate_above`] before it reports its log leadership term id again,
//! and [`TermsFile::set_candidate_term`] before the vote or request vote that raises
//! the candidate term is sent; each write syncs before it returns. A reader takes the
//! entries in file order, skips a deleted one and stops at the empty last entry a crash
//! left, an empty entry before the last being damage:
//! [`TermsHeader::parse`] and [`TermsEntry::parse`] are the checks of "What a reader
//! rejects", in order, which the vectors of `spec/vectors/durable-log.json` pin.
//!
//! A crash tears at most one write, and the reader reads each as the state before it.
//! The candidate term has two slots, each under its own checksum, and a raise writes the
//! slot the stored candidate term is not read from, so a torn raise reads as the vote
//! stored before it. An append syncs an entry's fields before it writes the state, so a
//! torn append reads as an empty entry, and a checksum that fails under a written state
//! is damage and never a crash's work. The file is created with its magic last, so a
//! torn creation is written again and is never taken for damage.
//!
//! Nothing here runs per message: the file is written twice per term and once per vote.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::layout::crc32;

/// The file's name in the log directory.
pub const FILE_NAME: &str = "terms";
/// The header's eight-byte magic, `CVLYTERM`.
pub const MAGIC: [u8; 8] = *b"CVLYTERM";
/// The one version this crate writes and reads.
pub const VERSION: i32 = 0;
/// The header's length in bytes.
pub const HEADER_LENGTH: usize = 64;
/// An entry's length in bytes.
pub const ENTRY_LENGTH: usize = 32;
/// The most entries the file holds, the catalog's bound on recordings.
pub const MAX_ENTRIES: u64 = 1_048_576;
/// The entries [`TermsFile::open`] reads at a time, so that its buffer is fixed whatever
/// the file's length.
const READ_ENTRIES: usize = 128;
/// The candidate term of a member that has voted in and proposed no term.
pub const NO_CANDIDATE_TERM: i64 = -1;
/// Where each candidate slot begins in the header: the term, then its checksum.
const SLOT_OFFSETS: [usize; 2] = [16, 32];
/// A candidate slot's length: the term, an i64, and its CRC32.
const SLOT_LENGTH: usize = 12;
/// The recording id of a term that has no recording yet.
pub const NO_RECORDING: i64 = -1;
/// An entry's state: empty. The file ends at an empty entry when it is the last whole
/// one, the torn tail of a crash; an empty entry before the last is damage, which
/// [`TermsFile::open`] checks, since the check needs the file's length.
pub const EMPTY: i32 = 0;
/// An entry's state: valid.
pub const VALID: i32 = 1;
/// An entry's state: deleted by a truncation.
pub const DELETED: i32 = 3;

/// Why a terms file, or one of its entries, was refused: the checks of
/// `spec/durable-log.md`, "What a reader rejects", by name, and the I/O errors around
/// them.
#[derive(Debug)]
pub enum TermsError {
    /// The file could not be opened, read, written or synced.
    Io {
        /// What was being done.
        what: &'static str,
        /// The file.
        path: PathBuf,
        /// The operating system's reason.
        source: io::Error,
    },
    /// The file is shorter than the header.
    TooShort {
        /// The length found.
        found: u64,
    },
    /// The magic is eight zero bytes: the creator has not finished. The writer writes a
    /// torn creation again, so [`TermsFile::open`] returns this only for a zero magic
    /// over a term or a vote, which creation never writes: damage.
    NotReady,
    /// The magic is not `CVLYTERM`.
    Magic,
    /// The version is not 0.
    Version {
        /// The version found.
        found: i32,
    },
    /// The entry length is not 32.
    EntryLength {
        /// The entry length found.
        found: i32,
    },
    /// Neither candidate slot's checksum matches, or a slot whose checksum matches
    /// holds a term below −1: damage, since a crash tears at most one slot. One slot
    /// whose checksum does not match is no error: it is the raise a crash tore.
    Candidate,
    /// The file holds more whole entries than the bound.
    Length {
        /// The whole entries found.
        found: u64,
    },
    /// An entry's state is not 0, 1 or 3.
    State {
        /// The state found.
        found: i32,
    },
    /// A valid or deleted entry's checksum does not match: damage wherever the entry
    /// is, since the writer syncs an entry's fields and checksum before it writes the
    /// state, so no crash leaves it. [`TermsFile::open`] reports it as
    /// [`TermsError::Damaged`], with the entry's index.
    Checksum,
    /// An entry of the file has a checksum that does not match: the file is damaged,
    /// it is left as it was, and no term is dropped.
    Damaged {
        /// The index of the entry whose checksum does not match.
        index: u64,
    },
    /// An entry of the file is empty and a whole entry follows it. With one writer only
    /// the last whole entry can be empty, the torn tail of a crash, so the file is
    /// damaged: it is left as it was, and no term is dropped. The `state` check.
    EmptyBeforeLast {
        /// The index of the empty entry.
        index: u64,
    },
    /// An entry's term is below 0 or not above the valid entry before it.
    Term {
        /// The term found.
        found: i64,
    },
    /// An entry's term base log position is below 0, not a multiple of 32 or below
    /// the valid entry's before it.
    Position {
        /// The position found.
        found: i64,
    },
    /// An entry's recording id is below −1.
    Recording {
        /// The recording id found.
        found: i64,
    },
    /// An append past the bound: `terms full`.
    Full,
    /// A write the file's rules refuse: nothing was written and the record of terms is
    /// as it was. The consensus module asked for the write, so the fault is Cavalry's,
    /// not the file's.
    Refused {
        /// The check the write fails, as "What a reader rejects" names it.
        check: &'static str,
        /// The value refused.
        found: i64,
        /// The rule the value breaks.
        rule: &'static str,
    },
}

impl TermsError {
    /// The check's name as the spec and the vectors spell it.
    pub const fn reason(&self) -> &'static str {
        match self {
            TermsError::Io { .. } => "io",
            TermsError::TooShort { .. } => "too-short",
            TermsError::NotReady => "not-ready",
            TermsError::Magic => "magic",
            TermsError::Version { .. } => "version",
            TermsError::EntryLength { .. } => "entry-length",
            TermsError::Candidate => "candidate",
            TermsError::Length { .. } => "length",
            TermsError::State { .. } | TermsError::EmptyBeforeLast { .. } => "state",
            TermsError::Checksum | TermsError::Damaged { .. } => "checksum",
            TermsError::Term { .. } => "term",
            TermsError::Position { .. } => "position",
            TermsError::Recording { .. } => "recording",
            TermsError::Full => "terms-full",
            TermsError::Refused { check, .. } => check,
        }
    }

    /// The reader's refusal of an entry, as the refusal of the write that would have
    /// stored it: the same check, and a message that says the file is unchanged.
    fn refused(self) -> Self {
        match self {
            TermsError::Term { found } => TermsError::Refused {
                check: "term",
                found,
                rule: "is below 0 or not above the last term recorded",
            },
            TermsError::Position { found } => TermsError::Refused {
                check: "position",
                found,
                rule: "is below 0, not a multiple of 32 or below the last term's base",
            },
            TermsError::Recording { found } => TermsError::Refused {
                check: "recording",
                found,
                rule: "is below -1",
            },
            other => other,
        }
    }
}

impl std::fmt::Display for TermsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const DOC: &str = "docs/reference/durable-log.md#terms";
        match self {
            TermsError::Io { what, path, source } => {
                write!(
                    f,
                    "terms file: cannot {what} {}: {source}; see {DOC}",
                    path.display()
                )
            }
            TermsError::TooShort { found } => write!(
                f,
                "terms file rejected (too-short): {found} bytes, under the 64 of the header; \
                 the file was torn while it was created, and the member writes it again when \
                 it starts, so start the member; see {DOC}"
            ),
            TermsError::NotReady => write!(
                f,
                "terms file not ready (not-ready): the magic is zero, so the file's creation \
                 did not finish. A member that is starting writes the file again: read it \
                 again. A member that refused to start with this found a term or a vote under \
                 the zero magic, so the file is damaged: restore it from the member's last \
                 good copy, or start a new log directory for this member; see {DOC}"
            ),
            TermsError::Magic => write!(
                f,
                "terms file rejected (magic): the first eight bytes are not CVLYTERM; the \
                 file is not a terms file, so move it out of the log directory; see {DOC}"
            ),
            TermsError::Version { found } => write!(
                f,
                "terms file rejected (version): version {found}, this reader knows 0; the \
                 file was written by another Cavalry version, upgrade this member; see {DOC}"
            ),
            TermsError::EntryLength { found } => write!(
                f,
                "terms file rejected (entry-length): entries of {found} bytes, this reader \
                 reads 32; the file was written by another Cavalry version; see {DOC}"
            ),
            TermsError::Candidate => write!(
                f,
                "terms file rejected (candidate): neither candidate term slot's checksum \
                 matches, or a slot holds a candidate term below -1, so the member's stored \
                 vote cannot be read and the file is damaged: restore it from the member's \
                 last good copy, or start a new log directory for this member; see {DOC}"
            ),
            TermsError::Length { found } => write!(
                f,
                "terms file rejected (length): {found} entries, over the {MAX_ENTRIES} the \
                 file holds; start a new log directory for this member; see {DOC}"
            ),
            TermsError::State { found } => write!(
                f,
                "terms entry rejected (state): state {found} is not 0, 1 or 3; the file is \
                 damaged past this entry, so restore it from the member's last good copy or \
                 start a new log directory; see {DOC}"
            ),
            TermsError::Checksum => write!(
                f,
                "terms entry rejected (checksum): the entry's checksum does not match its \
                 fields. The writer syncs the fields before the state, so no crash leaves \
                 this and the file is damaged: restore it from the member's last good copy, \
                 or start a new log directory for this member; see {DOC}"
            ),
            TermsError::Damaged { index } => write!(
                f,
                "terms file rejected (checksum): entry {index}'s checksum does not match its \
                 fields. The writer syncs the fields before the state, so no crash leaves \
                 this and the file is damaged; it was left as it was and no term was \
                 dropped: restore it from the member's last good copy, or start a new log \
                 directory for this member; see {DOC}"
            ),
            TermsError::EmptyBeforeLast { index } => write!(
                f,
                "terms file rejected (state): entry {index} is empty and an entry follows it. \
                 The writer appends an entry only after the one before it is whole, so only \
                 the last entry can be empty and the file is damaged; it was left as it was \
                 and no term was dropped: restore it from the member's last good copy, or \
                 start a new log directory for this member; see {DOC}"
            ),
            TermsError::Term { found } => write!(
                f,
                "terms entry rejected (term): term {found} is below 0 or not above the term \
                 before it; the file is damaged past this entry, so restore it or start a new \
                 log directory; see {DOC}"
            ),
            TermsError::Position { found } => write!(
                f,
                "terms entry rejected (position): term base log position {found} is below 0, \
                 not a multiple of 32 or below the position before it; the file is damaged past \
                 this entry, so restore it or start a new log directory; see {DOC}"
            ),
            TermsError::Recording { found } => write!(
                f,
                "terms entry rejected (recording): recording id {found} is below -1; the file \
                 is damaged past this entry, so restore it or start a new log directory; see \
                 {DOC}"
            ),
            TermsError::Full => write!(
                f,
                "terms full: the file holds {MAX_ENTRIES} entries, its bound; start a new log \
                 directory for this member; see {DOC}"
            ),
            TermsError::Refused { check, found, rule } => write!(
                f,
                "terms file refused a write ({check}): {found} {rule}; nothing was written, so \
                 the member's record of terms and its vote are as they were: keep this log \
                 directory, and report this as a bug in the consensus module with the output \
                 of `cavalry doctor`; see {DOC}"
            ),
        }
    }
}

impl std::error::Error for TermsError {}

/// The terms file's header, read or written: the two candidate term slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermsHeader {
    /// The candidate term each slot holds, −1 for none, or `None` for a slot whose
    /// checksum does not match, the raise a crash tore. A header that parsed holds a
    /// term in at least one slot.
    pub candidate_slots: [Option<i64>; 2],
}

impl TermsHeader {
    /// The header of a created file: both slots hold −1, no candidate term.
    pub const CREATED: Self = Self {
        candidate_slots: [Some(NO_CANDIDATE_TERM); 2],
    };

    /// The stored candidate term: the higher of the slots that hold one, or `None` when
    /// neither does, which a header that parsed never is.
    pub fn candidate_term(&self) -> Option<i64> {
        match self.candidate_slots {
            [Some(first), Some(second)] => Some(first.max(second)),
            [Some(term), None] | [None, Some(term)] => Some(term),
            [None, None] => None,
        }
    }

    /// The slot a raise writes: the one the stored candidate term is not read from,
    /// slot 1 when both slots hold it, so that a raise a crash tears leaves the stored
    /// candidate term as it was.
    pub fn raise_slot(&self) -> usize {
        match self.candidate_slots {
            [Some(first), Some(second)] if second > first => 0,
            [None, Some(_)] => 0,
            _ => 1,
        }
    }

    /// The header's bytes: the magic, version 0, entry length 32, the two candidate
    /// slots, the rest zero. A slot that holds no term is written with a checksum that
    /// does not match, as a torn raise leaves it.
    pub fn encode(&self) -> [u8; HEADER_LENGTH] {
        let mut bytes = [0u8; HEADER_LENGTH];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&(ENTRY_LENGTH as i32).to_le_bytes());
        for (slot, at) in self.candidate_slots.iter().zip(SLOT_OFFSETS) {
            bytes[at..at + SLOT_LENGTH].copy_from_slice(&encode_slot(*slot));
        }
        bytes
    }

    /// The checks of "What a reader rejects" on the header, in order, with the file's
    /// length: the first failure by name. One slot whose checksum does not match is no
    /// failure, the slot reads as `None`; neither matching, or a matching slot below −1,
    /// is [`TermsError::Candidate`].
    pub fn parse(bytes: &[u8], file_length: u64) -> Result<Self, TermsError> {
        if file_length < HEADER_LENGTH as u64 || bytes.len() < HEADER_LENGTH {
            return Err(TermsError::TooShort {
                found: file_length.min(bytes.len() as u64),
            });
        }
        if bytes[..8] == [0u8; 8] {
            return Err(TermsError::NotReady);
        }
        if bytes[..8] != MAGIC {
            return Err(TermsError::Magic);
        }
        let version = i32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4]));
        if version != VERSION {
            return Err(TermsError::Version { found: version });
        }
        let entry_length = i32::from_le_bytes(bytes[12..16].try_into().unwrap_or([0; 4]));
        if entry_length != ENTRY_LENGTH as i32 {
            return Err(TermsError::EntryLength {
                found: entry_length,
            });
        }
        let entries = Self::entries(file_length);
        if entries > MAX_ENTRIES {
            return Err(TermsError::Length { found: entries });
        }
        let candidate_slots = Self::slots(bytes);
        let below_none = |term: &i64| *term < NO_CANDIDATE_TERM;
        if candidate_slots == [None, None] || candidate_slots.iter().flatten().any(below_none) {
            return Err(TermsError::Candidate);
        }
        Ok(Self { candidate_slots })
    }

    /// The whole entries a file of `file_length` bytes holds after the header.
    pub const fn entries(file_length: u64) -> u64 {
        file_length.saturating_sub(HEADER_LENGTH as u64) / ENTRY_LENGTH as u64
    }

    /// The candidate slots of a header's bytes, whatever its magic: the term of each
    /// slot whose checksum matches.
    fn slots(bytes: &[u8]) -> [Option<i64>; 2] {
        SLOT_OFFSETS.map(|at| decode_slot(bytes, at))
    }
}

/// A candidate slot's bytes: the term and its checksum, or, for a slot that holds no
/// term, a zero term under a checksum that does not match.
fn encode_slot(slot: Option<i64>) -> [u8; SLOT_LENGTH] {
    let mut bytes = [0u8; SLOT_LENGTH];
    let checksum = match slot {
        Some(term) => {
            bytes[..8].copy_from_slice(&term.to_le_bytes());
            crc32(&bytes[..8])
        }
        None => !crc32(&bytes[..8]),
    };
    bytes[8..].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

/// The term of the candidate slot at `at`, when its checksum matches.
fn decode_slot(header: &[u8], at: usize) -> Option<i64> {
    let term = header.get(at..at + 8)?;
    let checksum = header.get(at + 8..at + SLOT_LENGTH)?;
    let matches = u32::from_le_bytes(checksum.try_into().ok()?) == crc32(term);
    matches.then(|| i64::from_le_bytes(term.try_into().unwrap_or([0; 8])))
}

/// One entry of the terms file: a term the member recorded, with its base and its
/// recording, in one of the three states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermsEntry {
    /// [`EMPTY`], [`VALID`] or [`DELETED`].
    pub state: i32,
    /// The leadership term id.
    pub leadership_term_id: i64,
    /// The term base log position.
    pub term_base_log_position: i64,
    /// The recording of the term in this directory, or [`NO_RECORDING`].
    pub recording_id: i64,
}

impl TermsEntry {
    /// The entry's bytes: the state, the checksum of the three fields, then the fields.
    pub fn encode(&self) -> [u8; ENTRY_LENGTH] {
        let mut bytes = [0u8; ENTRY_LENGTH];
        bytes[..4].copy_from_slice(&self.state.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.leadership_term_id.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.term_base_log_position.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.recording_id.to_le_bytes());
        let checksum = if self.state == EMPTY {
            0
        } else {
            crc32(&bytes[8..32])
        };
        bytes[4..8].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// The checks of "What a reader rejects" on an entry, in order, against the valid
    /// entry before it: the first failure by name. An empty entry parses as itself,
    /// its other fields unread: the file's end only as the last whole entry, which
    /// [`TermsFile::open`] checks against the file's length, refusing one before the
    /// last as [`TermsError::EmptyBeforeLast`].
    pub fn parse(bytes: &[u8], previous: Option<&TermsEntry>) -> Result<Self, TermsError> {
        if bytes.len() < ENTRY_LENGTH {
            return Err(TermsError::TooShort {
                found: bytes.len() as u64,
            });
        }
        let state = i32::from_le_bytes(bytes[..4].try_into().unwrap_or([0; 4]));
        if state == EMPTY {
            return Ok(Self {
                state,
                leadership_term_id: 0,
                term_base_log_position: 0,
                recording_id: 0,
            });
        }
        if state != VALID && state != DELETED {
            return Err(TermsError::State { found: state });
        }
        let checksum = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4]));
        if checksum != crc32(&bytes[8..32]) {
            return Err(TermsError::Checksum);
        }
        let leadership_term_id = i64::from_le_bytes(bytes[8..16].try_into().unwrap_or([0; 8]));
        let term_base_log_position = i64::from_le_bytes(bytes[16..24].try_into().unwrap_or([0; 8]));
        let recording_id = i64::from_le_bytes(bytes[24..32].try_into().unwrap_or([0; 8]));
        if leadership_term_id < 0
            || previous.is_some_and(|before| leadership_term_id <= before.leadership_term_id)
        {
            return Err(TermsError::Term {
                found: leadership_term_id,
            });
        }
        if term_base_log_position < 0
            || term_base_log_position % 32 != 0
            || previous.is_some_and(|before| term_base_log_position < before.term_base_log_position)
        {
            return Err(TermsError::Position {
                found: term_base_log_position,
            });
        }
        if recording_id < NO_RECORDING {
            return Err(TermsError::Recording {
                found: recording_id,
            });
        }
        Ok(Self {
            state,
            leadership_term_id,
            term_base_log_position,
            recording_id,
        })
    }
}

/// The terms file, open for writing by the consensus module: the valid entries it
/// holds, the candidate term, and the writes of "The driver", each synced before it
/// returns.
#[derive(Debug)]
pub struct TermsFile {
    path: PathBuf,
    file: File,
    /// The candidate slots as the file holds them, which choose the slot a raise writes.
    header: TermsHeader,
    candidate_term: i64,
    /// Every entry in file order, deleted ones included, so that an index is a file
    /// offset; bounded by [`MAX_ENTRIES`].
    entries: Vec<TermsEntry>,
}

impl TermsFile {
    /// The file's path in `dir`.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(FILE_NAME)
    }

    /// Creates the file in `dir` with its header, the candidate term −1 and no entry:
    /// every field after the magic, synced, then the magic, synced, so that a creation a
    /// crash tears is told from damage by its zero magic. A file already there is
    /// [`TermsError::Io`].
    pub fn create(dir: &Path) -> Result<Self, TermsError> {
        let path = Self::path_in(dir);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| TermsError::Io {
                what: "create",
                path: path.clone(),
                source,
            })?;
        Self::write_header(&file, &path)?;
        Ok(Self::created(path, file))
    }

    /// A file that holds the created header and no entry.
    fn created(path: PathBuf, file: File) -> Self {
        Self {
            path,
            file,
            header: TermsHeader::CREATED,
            candidate_term: NO_CANDIDATE_TERM,
            entries: Vec::new(),
        }
    }

    /// Writes the created header over whatever the file holds, the way the catalog's is
    /// written (`spec/durable-log.md`, "The driver"): the file emptied, every field
    /// after the magic and a sync, then the magic and a sync.
    fn write_header(file: &File, path: &Path) -> Result<(), TermsError> {
        let io = |what: &'static str| {
            let path = path.to_path_buf();
            move |source| TermsError::Io { what, path, source }
        };
        let encoded = TermsHeader::CREATED.encode();
        file.set_len(0).map_err(io("empty"))?;
        file.write_all_at(&encoded[8..], 8)
            .map_err(io("write the header of"))?;
        file.sync_all().map_err(io("sync"))?;
        file.write_all_at(&encoded[..8], 0)
            .map_err(io("write the magic of"))?;
        file.sync_all().map_err(io("sync"))
    }

    /// Opens the file in `dir`, reading its header and every entry up to an empty last
    /// one, which a torn append left and the next write overwrites, and creates the file
    /// when there is none. A file
    /// a crash tore while it was created, shorter than its header or with a zero magic
    /// over no term and no vote, is written again; a zero magic over a term or a vote is
    /// [`TermsError::NotReady`]. A candidate slot whose checksum does not match is the
    /// raise a crash tore, and the candidate term is the other slot's. An entry whose
    /// checksum fails is [`TermsError::Damaged`], the last entry's too, and an empty
    /// entry with a whole entry after it is [`TermsError::EmptyBeforeLast`]. The
    /// file's length is checked against the bound before a byte is read, and the entries
    /// are read 128 at a time into a fixed buffer, so that what the reader holds is
    /// bounded by [`MAX_ENTRIES`] whatever is on disk.
    pub fn open(dir: &Path) -> Result<Self, TermsError> {
        let path = Self::path_in(dir);
        if !path.exists() {
            return Self::create(dir);
        }
        let io = |what: &'static str| {
            let path = path.clone();
            move |source| TermsError::Io { what, path, source }
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(io("open"))?;
        // The bound from the length alone, before a byte is read.
        let file_length = file.metadata().map_err(io("stat"))?.len();
        let whole = TermsHeader::entries(file_length);
        if whole > MAX_ENTRIES {
            return Err(TermsError::Length { found: whole });
        }
        let mut header = [0u8; HEADER_LENGTH];
        if file_length >= HEADER_LENGTH as u64 {
            file.read_exact_at(&mut header, 0)
                .map_err(io("read the header of"))?;
        }
        // A creation a crash tore: creation writes no entry and no candidate term but
        // −1, so a zero magic over either is not one, and the header's parse refuses it.
        let voted = |term: &i64| *term > NO_CANDIDATE_TERM;
        let torn_creation = file_length < HEADER_LENGTH as u64
            || (header[..8] == [0u8; 8]
                && whole == 0
                && !TermsHeader::slots(&header).iter().flatten().any(voted));
        if torn_creation {
            Self::write_header(&file, &path)?;
            return Ok(Self::created(path, file));
        }
        let header = TermsHeader::parse(&header, file_length)?;
        let Some(candidate_term) = header.candidate_term() else {
            return Err(TermsError::Candidate);
        };
        let mut entries = Vec::new();
        let mut previous: Option<TermsEntry> = None;
        let mut chunk = [0u8; READ_ENTRIES * ENTRY_LENGTH];
        let mut index = 0;
        'read: while index < whole {
            let count = (whole - index).min(READ_ENTRIES as u64) as usize;
            let bytes = &mut chunk[..count * ENTRY_LENGTH];
            let at = HEADER_LENGTH as u64 + index * ENTRY_LENGTH as u64;
            file.read_exact_at(bytes, at).map_err(io("read"))?;
            for (offset, entry_bytes) in bytes.as_chunks::<ENTRY_LENGTH>().0.iter().enumerate() {
                let entry = match TermsEntry::parse(entry_bytes, previous.as_ref()) {
                    Ok(entry) => entry,
                    Err(TermsError::Checksum) => {
                        // Damage wherever it is: an append syncs the fields before the
                        // state, so no crash leaves a written state over a checksum
                        // that fails. The file is left as it was.
                        return Err(TermsError::Damaged {
                            index: index + offset as u64,
                        });
                    }
                    Err(error) => return Err(error),
                };
                if entry.state == EMPTY {
                    // The file's end only as its last whole entry, the torn tail of a
                    // crash. An append writes an entry only after the one before it is
                    // whole, so an empty entry with a whole entry after it is damage,
                    // and the terms after it are not dropped.
                    let empty = index + offset as u64;
                    if empty + 1 < whole {
                        return Err(TermsError::EmptyBeforeLast { index: empty });
                    }
                    break 'read;
                }
                if entry.state == VALID {
                    previous = Some(entry);
                }
                entries.push(entry);
            }
            index += count as u64;
        }
        Ok(Self {
            path,
            file,
            header,
            candidate_term,
            entries,
        })
    }

    /// The candidate term stored, −1 for none.
    pub const fn candidate_term(&self) -> i64 {
        self.candidate_term
    }

    /// The valid entries in order: the member's record of recorded terms.
    pub fn terms(&self) -> impl Iterator<Item = &TermsEntry> {
        self.entries.iter().filter(|entry| entry.state == VALID)
    }

    /// The highest term recorded, the member's log leadership term id, or −1 for none.
    pub fn log_leadership_term_id(&self) -> i64 {
        self.terms()
            .last()
            .map_or(-1, |entry| entry.leadership_term_id)
    }

    /// Appends a term in two synced writes: its checksum and fields, then its state.
    /// The term must be above the last valid one and its base at least the last valid
    /// base; past [`MAX_ENTRIES`] it is [`TermsError::Full`]. The recording id is final
    /// here, [`NO_RECORDING`] for a term whose recording does not exist yet: an entry's
    /// fields are written once, and only its state changes after, so that no write but
    /// the last append can tear an entry (ADR 0060, Alternatives).
    pub fn append(
        &mut self,
        leadership_term_id: i64,
        term_base_log_position: i64,
        recording_id: i64,
    ) -> Result<(), TermsError> {
        if self.entries.len() as u64 >= MAX_ENTRIES {
            return Err(TermsError::Full);
        }
        let entry = TermsEntry {
            state: VALID,
            leadership_term_id,
            term_base_log_position,
            recording_id,
        };
        let last = self.terms().last().copied();
        TermsEntry::parse(&entry.encode(), last.as_ref()).map_err(TermsError::refused)?;
        let bytes = entry.encode();
        let at = (HEADER_LENGTH + self.entries.len() * ENTRY_LENGTH) as u64;
        // The fields and the checksum first, the state last, so that a crash between
        // the two leaves an empty entry.
        self.write_at(at + 4, &bytes[4..], "append")?;
        self.write_at(at, &bytes[..4], "append")?;
        self.entries.push(entry);
        Ok(())
    }

    /// Truncates: every valid term whose base is above `position` is deleted, from the
    /// last backwards, each write synced, so that a truncation drops the recorded terms
    /// above the position (`spec/cluster.md`, "The log").
    pub fn truncate_above(&mut self, position: i64) -> Result<(), TermsError> {
        for index in (0..self.entries.len()).rev() {
            let entry = self.entries[index];
            if entry.state != VALID || entry.term_base_log_position <= position {
                continue;
            }
            let at = (HEADER_LENGTH + index * ENTRY_LENGTH) as u64;
            self.write_at(at, &DELETED.to_le_bytes(), "truncate")?;
            self.entries[index].state = DELETED;
        }
        Ok(())
    }

    /// Raises the candidate term in one synced write: the term and its checksum, in the
    /// slot the stored candidate term is not read from, so that a crash during the write
    /// leaves the candidate term stored before it. A value below −1 or below the
    /// candidate term stored is refused, since the module only raises it ("The driver");
    /// the value stored already needs no write.
    pub fn set_candidate_term(&mut self, candidate_term: i64) -> Result<(), TermsError> {
        if candidate_term < NO_CANDIDATE_TERM || candidate_term < self.candidate_term {
            return Err(TermsError::Refused {
                check: "candidate",
                found: candidate_term,
                rule: "is below -1 or below the candidate term stored",
            });
        }
        if candidate_term == self.candidate_term {
            return Ok(());
        }
        let slot = self.header.raise_slot();
        let bytes = encode_slot(Some(candidate_term));
        self.write_at(SLOT_OFFSETS[slot] as u64, &bytes, "store")?;
        self.header.candidate_slots[slot] = Some(candidate_term);
        self.candidate_term = candidate_term;
        Ok(())
    }

    fn write_at(&mut self, at: u64, bytes: &[u8], what: &'static str) -> Result<(), TermsError> {
        let io = |what: &'static str| {
            let path = self.path.clone();
            move |source| TermsError::Io { what, path, source }
        };
        self.file.seek(SeekFrom::Start(at)).map_err(io(what))?;
        self.file.write_all(bytes).map_err(io(what))?;
        self.file.sync_all().map_err(io("sync"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The candidate slots of a terms file's bytes.
    fn read_slots(bytes: &[u8]) -> [Option<i64>; 2] {
        TermsHeader::slots(&bytes[..HEADER_LENGTH])
    }

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cavalry-terms-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory");
        dir
    }

    #[test]
    fn a_created_file_reads_back_empty_and_an_appended_term_reads_back() {
        let dir = dir("round-trip");
        let mut file = TermsFile::create(&dir).expect("created");
        assert_eq!(file.candidate_term(), NO_CANDIDATE_TERM);
        assert_eq!(file.log_leadership_term_id(), -1);
        file.append(2, 0, NO_RECORDING).expect("appended");
        file.append(3, 4096, 1).expect("appended");
        file.set_candidate_term(4).expect("stored");
        drop(file);
        let file = TermsFile::open(&dir).expect("opened");
        let terms: Vec<_> = file.terms().copied().collect();
        assert_eq!(terms.len(), 2);
        assert_eq!(
            (
                terms[0].leadership_term_id,
                terms[0].term_base_log_position,
                terms[0].recording_id
            ),
            (2, 0, NO_RECORDING)
        );
        assert_eq!(
            (
                terms[1].leadership_term_id,
                terms[1].term_base_log_position,
                terms[1].recording_id
            ),
            (3, 4096, 1)
        );
        assert_eq!(file.candidate_term(), 4);
        assert_eq!(file.log_leadership_term_id(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncation_deletes_the_terms_above_the_position_and_the_next_append_follows_them() {
        let dir = dir("truncate");
        let mut file = TermsFile::create(&dir).expect("created");
        file.append(2, 0, 0).expect("appended");
        file.append(3, 4096, 1).expect("appended");
        file.append(4, 8192, 2).expect("appended");
        file.truncate_above(4096).expect("truncated");
        assert_eq!(file.log_leadership_term_id(), 3);
        file.append(5, 4096, 3)
            .expect("a term after the deleted one");
        drop(file);
        let file = TermsFile::open(&dir).expect("opened");
        let terms: Vec<i64> = file.terms().map(|entry| entry.leadership_term_id).collect();
        assert_eq!(terms, vec![2, 3, 5]);
        assert_eq!(file.entries.len(), 4, "the deleted entry keeps its place");
        // A truncation writes the state and nothing else: the fields and their checksum
        // are the bytes the append wrote, so the entry's one write after its append
        // cannot tear it.
        let bytes = std::fs::read(TermsFile::path_in(&dir)).expect("read");
        let at = HEADER_LENGTH + 2 * ENTRY_LENGTH;
        let appended = TermsEntry {
            state: VALID,
            leadership_term_id: 4,
            term_base_log_position: 8192,
            recording_id: 2,
        }
        .encode();
        assert_eq!(&bytes[at..at + 4], &DELETED.to_le_bytes());
        assert_eq!(&bytes[at + 4..at + ENTRY_LENGTH], &appended[4..]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_torn_tail_ends_the_file_and_a_torn_raise_reads_as_the_candidate_term_before_it() {
        let dir = dir("torn");
        let mut file = TermsFile::create(&dir).expect("created");
        file.append(2, 0, 0).expect("appended");
        file.set_candidate_term(7).expect("stored");
        drop(file);
        let path = TermsFile::path_in(&dir);
        let mut bytes = std::fs::read(&path).expect("read");
        // A second entry with its fields written and its state not yet: empty.
        let mut torn = TermsEntry {
            state: VALID,
            leadership_term_id: 3,
            term_base_log_position: 4096,
            recording_id: 1,
        }
        .encode();
        torn[..4].copy_from_slice(&EMPTY.to_le_bytes());
        bytes.extend_from_slice(&torn);
        // The created file's first raise wrote slot 1, so the raise to 9 writes slot 0.
        // A crash tears it: the term is written and its checksum is not.
        assert_eq!(read_slots(&bytes), [Some(NO_CANDIDATE_TERM), Some(7)]);
        bytes[16..24].copy_from_slice(&9i64.to_le_bytes());
        std::fs::write(&path, &bytes).expect("written");
        let mut file = TermsFile::open(&dir).expect("opened");
        assert_eq!(file.terms().count(), 1);
        assert_eq!(
            file.candidate_term(),
            7,
            "the vote stored before the torn raise, not none"
        );
        // The next raise writes the torn slot again, and the vote before it stays.
        file.set_candidate_term(9)
            .expect("raised over the torn slot");
        drop(file);
        let mut bytes = std::fs::read(&path).expect("read");
        assert_eq!(read_slots(&bytes), [Some(9), Some(7)]);
        // A raise to 11 torn in slot 1, the other way: the checksum written, the term not.
        let raised = encode_slot(Some(11));
        bytes[40..44].copy_from_slice(&raised[8..]);
        std::fs::write(&path, &bytes).expect("written");
        let mut file = TermsFile::open(&dir).expect("opened");
        assert_eq!(
            file.candidate_term(),
            9,
            "the vote stored before the torn raise"
        );
        file.set_candidate_term(11)
            .expect("raised over the torn slot");
        drop(file);
        let bytes = std::fs::read(&path).expect("read");
        assert_eq!(read_slots(&bytes), [Some(9), Some(11)]);
        assert_eq!(TermsFile::open(&dir).expect("opened").candidate_term(), 11);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn neither_candidate_slot_or_a_slot_below_none_is_damage_with_something_to_do() {
        let dir = dir("candidate-damage");
        let mut file = TermsFile::create(&dir).expect("created");
        file.set_candidate_term(7).expect("stored");
        drop(file);
        let path = TermsFile::path_in(&dir);
        let whole = std::fs::read(&path).expect("read");
        // Both checksums fail: no crash leaves that, and the stored vote cannot be read.
        let mut bytes = whole.clone();
        bytes[24] ^= 0xFF;
        bytes[40] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("written");
        let refused = TermsFile::open(&dir).expect_err("neither slot");
        assert_eq!(refused.reason(), "candidate");
        let message = refused.to_string();
        assert!(message.contains("damaged"), "{message}");
        assert!(message.contains("restore it"), "{message}");
        assert!(!message.contains("nothing to do"), "{message}");
        // A slot whose checksum matches a term below -1: a term no writer stores.
        let mut bytes = whole;
        bytes[16..28].copy_from_slice(&encode_slot(Some(-5)));
        std::fs::write(&path, &bytes).expect("written");
        assert_eq!(TermsFile::open(&dir).unwrap_err().reason(), "candidate");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            bytes,
            "a refused file is left as it was"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_creation_torn_by_a_crash_is_written_again() {
        let dir = dir("torn-creation");
        let path = TermsFile::path_in(&dir);
        let created = TermsHeader::CREATED.encode();
        let mut no_magic = created;
        no_magic[..8].fill(0);
        // Shorter than the header; every field written and the magic not; and a file of
        // zeros, grown before a byte of it was written.
        for torn in [&created[..40], &no_magic[..], &[0u8; HEADER_LENGTH][..]] {
            std::fs::write(&path, torn).expect("written");
            let mut file = TermsFile::open(&dir).expect("a torn creation opens");
            assert_eq!(file.candidate_term(), NO_CANDIDATE_TERM);
            assert_eq!(file.terms().count(), 0);
            assert_eq!(std::fs::read(&path).expect("read"), created);
            // The file written again is a whole one: it takes a term and a vote.
            file.append(2, 0, 0).expect("appended");
            file.set_candidate_term(3).expect("stored");
            drop(file);
            let file = TermsFile::open(&dir).expect("opened");
            assert_eq!(
                (file.log_leadership_term_id(), file.candidate_term()),
                (2, 3)
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_zero_magic_over_a_term_or_a_vote_is_not_ready_and_the_file_is_left_alone() {
        let dir = dir("not-ready");
        let path = TermsFile::path_in(&dir);
        let entry = TermsEntry {
            state: VALID,
            leadership_term_id: 2,
            term_base_log_position: 0,
            recording_id: 0,
        };
        // A term under a zero magic.
        let mut over_a_term = TermsHeader::CREATED.encode().to_vec();
        over_a_term[..8].fill(0);
        over_a_term.extend_from_slice(&entry.encode());
        // A vote under a zero magic: creation writes -1 in both slots and nothing else.
        let voted = TermsHeader {
            candidate_slots: [Some(NO_CANDIDATE_TERM), Some(4)],
        };
        let mut over_a_vote = voted.encode().to_vec();
        over_a_vote[..8].fill(0);
        for damaged in [over_a_term, over_a_vote] {
            std::fs::write(&path, &damaged).expect("written");
            let refused = TermsFile::open(&dir).expect_err("not a torn creation");
            assert_eq!(refused.reason(), "not-ready");
            assert!(refused.to_string().contains("damaged"), "{refused}");
            assert_eq!(
                std::fs::read(&path).expect("read"),
                damaged,
                "the term or the vote is not written over"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_checksum_that_fails_is_damage_whatever_follows_it_and_drops_no_term() {
        let dir = dir("damaged-entry");
        let mut file = TermsFile::create(&dir).expect("created");
        file.append(2, 0, 0).expect("appended");
        file.append(3, 4096, 1).expect("appended");
        file.append(4, 8192, 2).expect("appended");
        drop(file);
        let path = TermsFile::path_in(&dir);
        let mut bytes = std::fs::read(&path).expect("read");
        // Entry 1's checksum fails and entry 2 follows it: the writer syncs an entry's
        // fields before its state, so no crash leaves this.
        bytes[HEADER_LENGTH + ENTRY_LENGTH + 4] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("written");
        let refused = TermsFile::open(&dir).expect_err("damage");
        assert_eq!(refused.reason(), "checksum");
        assert!(
            matches!(refused, TermsError::Damaged { index: 1 }),
            "{refused:?}"
        );
        let message = refused.to_string();
        assert!(message.contains("damaged"), "{message}");
        assert!(!message.contains("nothing to do"), "{message}");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            bytes,
            "term 4 is still in the file"
        );
        // With an empty entry after it the answer is the same: what follows an entry
        // does not make its failing checksum a crash's work.
        bytes[HEADER_LENGTH + 2 * ENTRY_LENGTH..][..4].copy_from_slice(&EMPTY.to_le_bytes());
        std::fs::write(&path, &bytes).expect("written");
        let refused = TermsFile::open(&dir).expect_err("damage");
        assert!(
            matches!(refused, TermsError::Damaged { index: 1 }),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_entry_before_the_last_is_damage_and_the_file_is_left_as_it_was() {
        let dir = dir("empty-before-last");
        let mut file = TermsFile::create(&dir).expect("created");
        file.append(2, 0, 0).expect("appended");
        file.append(3, 4096, 1).expect("appended");
        file.append(4, 8192, 2).expect("appended");
        drop(file);
        let path = TermsFile::path_in(&dir);
        let mut bytes = std::fs::read(&path).expect("read");
        // Entry 1's state reads 0, which no checksum covers, and entry 2 follows it. The
        // writer appended entry 2 only after entry 1 was whole, so this is no torn tail:
        // the member may have reported append positions in terms 3 and 4.
        bytes[HEADER_LENGTH + ENTRY_LENGTH..][..4].copy_from_slice(&EMPTY.to_le_bytes());
        std::fs::write(&path, &bytes).expect("written");
        let refused = TermsFile::open(&dir).expect_err("damage");
        assert_eq!(refused.reason(), "state");
        assert!(
            matches!(refused, TermsError::EmptyBeforeLast { index: 1 }),
            "{refused:?}"
        );
        let message = refused.to_string();
        assert!(message.contains("damaged"), "{message}");
        assert!(message.contains("restore it"), "{message}");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            bytes,
            "terms 3 and 4 are still in the file, and nothing was written over them"
        );
        // The same entry empty as the file's last whole entry is the torn tail of a
        // crash: the file opens, and the next append takes the entry's place.
        bytes.truncate(HEADER_LENGTH + 2 * ENTRY_LENGTH);
        std::fs::write(&path, &bytes).expect("written");
        let mut file = TermsFile::open(&dir).expect("a torn tail opens");
        assert_eq!(file.log_leadership_term_id(), 2);
        file.append(5, 4096, 3)
            .expect("appended over the torn tail");
        drop(file);
        let file = TermsFile::open(&dir).expect("opened");
        let terms: Vec<i64> = file.terms().map(|entry| entry.leadership_term_id).collect();
        assert_eq!(terms, vec![2, 5]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_last_entry_whose_checksum_fails_is_damage_and_the_file_is_left_as_it_was() {
        let dir = dir("damaged-tail");
        let mut file = TermsFile::create(&dir).expect("created");
        file.append(2, 0, 0).expect("appended");
        file.append(3, 4096, 1).expect("appended");
        drop(file);
        let path = TermsFile::path_in(&dir);
        let mut bytes = std::fs::read(&path).expect("read");
        // The last entry's state is 1 and its fields whole: only its checksum fails. The
        // append synced the fields before the state, so this is no torn tail: the member
        // may have reported append positions in term 3, and the term must not be dropped.
        bytes[HEADER_LENGTH + ENTRY_LENGTH + 4] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("written");
        let refused = TermsFile::open(&dir).expect_err("damage");
        assert_eq!(refused.reason(), "checksum");
        assert!(
            matches!(refused, TermsError::Damaged { index: 1 }),
            "{refused:?}"
        );
        let message = refused.to_string();
        assert!(message.contains("damaged"), "{message}");
        assert!(message.contains("restore it"), "{message}");
        assert!(!message.contains("nothing to do"), "{message}");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            bytes,
            "term 3 is still in the file, and nothing was written over it"
        );
        // The entry's parse says the same, for a reader that holds one entry.
        let entry = &bytes[HEADER_LENGTH + ENTRY_LENGTH..];
        let parsed = TermsEntry::parse(entry, None).expect_err("damage");
        assert_eq!(parsed.reason(), "checksum");
        assert!(!parsed.to_string().contains("nothing to do"), "{parsed}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_over_the_bound_is_length_before_an_entry_is_read_and_before_its_candidate_slots() {
        let dir = dir("bound");
        let mut file = TermsFile::create(&dir).expect("created");
        file.append(2, 0, 0).expect("appended");
        file.set_candidate_term(7).expect("stored");
        drop(file);
        let path = TermsFile::path_in(&dir);
        // One whole entry past the bound, sparse: no byte past the first entry is written,
        // and the reader refuses from the length alone.
        let over = HEADER_LENGTH as u64 + (MAX_ENTRIES + 1) * ENTRY_LENGTH as u64;
        let grown = OpenOptions::new().write(true).open(&path).expect("opened");
        grown.set_len(over).expect("grown");
        let refused = TermsFile::open(&dir).expect_err("over the bound");
        assert_eq!(refused.reason(), "length");
        // Both candidate checksums torn too: `length` is checked before `candidate`, so
        // the file is still `length`, from the header's parse as from the open.
        grown.write_all_at(&[0xFF], 24).expect("torn");
        grown.write_all_at(&[0xFF], 40).expect("torn");
        drop(grown);
        assert_eq!(TermsFile::open(&dir).unwrap_err().reason(), "length");
        let header = TermsHeader {
            candidate_slots: [None, None],
        }
        .encode();
        assert_eq!(
            TermsHeader::parse(&header, over).unwrap_err().reason(),
            "length"
        );
        assert_eq!(
            TermsHeader::parse(&header, HEADER_LENGTH as u64)
                .unwrap_err()
                .reason(),
            "candidate"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_reader_takes_entries_across_its_read_buffer_up_to_the_torn_tail() {
        let dir = dir("chunks");
        let stored = TermsHeader {
            candidate_slots: [Some(3), Some(NO_CANDIDATE_TERM)],
        };
        let mut bytes = stored.encode().to_vec();
        for term in 0..300 {
            let entry = TermsEntry {
                state: VALID,
                leadership_term_id: term,
                term_base_log_position: term * 4096,
                recording_id: term,
            };
            bytes.extend_from_slice(&entry.encode());
        }
        // The last entry, in the reader's third buffer, as a torn append leaves it: its
        // fields and checksum written and its state not yet.
        bytes[HEADER_LENGTH + 299 * ENTRY_LENGTH..][..4].copy_from_slice(&EMPTY.to_le_bytes());
        std::fs::write(TermsFile::path_in(&dir), &bytes).expect("written");
        let file = TermsFile::open(&dir).expect("opened");
        assert_eq!(file.terms().count(), 299);
        assert_eq!(file.log_leadership_term_id(), 298);
        assert_eq!(file.candidate_term(), 3);
        drop(file);
        // An empty entry at the end of the reader's first buffer, its follower in the
        // second: damage, found though the entry after it is in another read.
        let mut holed = bytes.clone();
        holed[HEADER_LENGTH + (READ_ENTRIES - 1) * ENTRY_LENGTH..][..4]
            .copy_from_slice(&EMPTY.to_le_bytes());
        std::fs::write(TermsFile::path_in(&dir), &holed).expect("written");
        let refused = TermsFile::open(&dir).expect_err("damage");
        let hole = READ_ENTRIES as u64 - 1;
        assert!(
            matches!(refused, TermsError::EmptyBeforeLast { index } if index == hole),
            "{refused:?}"
        );
        // A checksum that fails in the reader's second buffer is damage, with its index
        // in the file and not in the buffer.
        bytes[HEADER_LENGTH + (READ_ENTRIES + 72) * ENTRY_LENGTH + 4] ^= 0xFF;
        std::fs::write(TermsFile::path_in(&dir), &bytes).expect("written");
        let refused = TermsFile::open(&dir).expect_err("damage");
        let damaged = READ_ENTRIES as u64 + 72;
        assert!(
            matches!(refused, TermsError::Damaged { index } if index == damaged),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_append_out_of_order_is_refused_and_the_bound_is_terms_full() {
        let dir = dir("refused");
        let mut file = TermsFile::create(&dir).expect("created");
        file.append(3, 4096, 1).expect("appended");
        assert_eq!(file.append(3, 4096, 2).unwrap_err().reason(), "term");
        assert_eq!(file.append(4, 0, 2).unwrap_err().reason(), "position");
        assert_eq!(file.append(4, 100, 2).unwrap_err().reason(), "position");
        assert_eq!(file.append(4, 4096, -2).unwrap_err().reason(), "recording");
        // A refusal is the caller's fault and the file is whole: the message says so, and
        // never tells the operator to give up the directory that holds the member's vote.
        let message = file.append(3, 4096, 2).unwrap_err().to_string();
        assert!(message.contains("nothing was written"), "{message}");
        assert!(!message.contains("new log directory"), "{message}");
        drop(file);
        let mut file = TermsFile::open(&dir).expect("opened");
        assert_eq!(
            file.terms().count(),
            1,
            "a refused write left the file alone"
        );
        file.entries.resize(MAX_ENTRIES as usize, file.entries[0]);
        assert_eq!(file.append(5, 4096, 2).unwrap_err().reason(), "terms-full");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_candidate_term_is_only_raised() {
        let dir = dir("candidate");
        let mut file = TermsFile::create(&dir).expect("created");
        let below = file.set_candidate_term(-2).expect_err("below -1");
        assert_eq!(below.reason(), "candidate");
        assert!(below.to_string().contains("nothing was written"), "{below}");
        file.set_candidate_term(5).expect("raised");
        assert_eq!(
            file.set_candidate_term(4)
                .expect_err("below the stored")
                .reason(),
            "candidate"
        );
        file.set_candidate_term(5).expect("the value stored");
        drop(file);
        let mut file = TermsFile::open(&dir).expect("opened");
        assert_eq!(file.candidate_term(), 5, "the refusals wrote nothing");
        // Each raise writes the slot the stored term is not read from, so the term
        // stored before a raise is on disk until the raise after it.
        let path = TermsFile::path_in(&dir);
        let slots = |path: &Path| read_slots(&std::fs::read(path).expect("read"));
        assert_eq!(slots(&path), [Some(NO_CANDIDATE_TERM), Some(5)]);
        file.set_candidate_term(8).expect("raised");
        assert_eq!(slots(&path), [Some(8), Some(5)]);
        file.set_candidate_term(9).expect("raised");
        assert_eq!(slots(&path), [Some(8), Some(9)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
