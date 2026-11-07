//! The terms file: a cluster member's record of the terms it has recorded and the
//! candidate term it has stored (`spec/durable-log.md`, version 2, "Terms"; ADR 0060).
//!
//! The consensus module of `cavalry-raft` is the file's only writer, from its own
//! thread: [`TermsFile::create`] or [`TermsFile::open`] at the member's start, then
//! [`TermsFile::append`] before the member reports an append position for a term,
//! [`TermsFile::truncate_above`] before it reports its log leadership term id again,
//! and [`TermsFile::set_candidate_term`] before the vote or request vote that raises
//! the candidate term is sent; each write syncs before it returns. A reader takes the
//! entries in file order, skips a deleted one and stops at the torn tail of a crash:
//! [`TermsHeader::parse`] and [`TermsEntry::parse`] are the checks of "What a reader
//! rejects", in order, which the vectors of `spec/vectors/durable-log.json` pin.
//!
//! Nothing here runs per message: the file is written once per term and once per vote.

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
/// The recording id of a term that has no recording yet.
pub const NO_RECORDING: i64 = -1;
/// An entry's state: empty, the file ends here.
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
    /// The candidate checksum does not match: the candidate term reads as −1 and the
    /// reader goes on, so a reader reports this and keeps the header.
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
    /// A valid or deleted entry's checksum does not match: the file ends here.
    Checksum,
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
            TermsError::Magic => "magic",
            TermsError::Version { .. } => "version",
            TermsError::EntryLength { .. } => "entry-length",
            TermsError::Candidate => "candidate",
            TermsError::Length { .. } => "length",
            TermsError::State { .. } => "state",
            TermsError::Checksum => "checksum",
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
                 the file was torn while it was created, so remove it and start the member \
                 again; see {DOC}"
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
                "terms file: the candidate term's checksum does not match, so the candidate \
                 term reads as -1, the write it was torn in being the last; nothing to do; \
                 see {DOC}"
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
                "terms entry rejected (checksum): the entry's checksum does not match, the \
                 torn tail of a crash, which the next write overwrites; nothing to do; see \
                 {DOC}"
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

/// The terms file's header, read or written: the candidate term with its checksum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermsHeader {
    /// The candidate term stored, −1 for none.
    pub candidate_term: i64,
}

impl TermsHeader {
    /// The header's bytes: the magic, version 0, entry length 32, the candidate term and
    /// its checksum, the rest zero.
    pub fn encode(&self) -> [u8; HEADER_LENGTH] {
        let mut bytes = [0u8; HEADER_LENGTH];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&(ENTRY_LENGTH as i32).to_le_bytes());
        bytes[16..24].copy_from_slice(&self.candidate_term.to_le_bytes());
        let checksum = crc32(&bytes[16..24]);
        bytes[24..28].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// The checks of "What a reader rejects" on the header, in order, with the file's
    /// length: the first failure by name. A candidate checksum that does not match is
    /// [`TermsError::Candidate`], after which a reader keeps the header with a candidate
    /// term of −1, as [`TermsFile::open`] does. The reader goes on past `candidate`, so
    /// the `length` check runs after it all the same, and a file over the bound is
    /// [`TermsError::Length`] whatever its candidate checksum.
    pub fn parse(bytes: &[u8], file_length: u64) -> Result<Self, TermsError> {
        if file_length < HEADER_LENGTH as u64 || bytes.len() < HEADER_LENGTH {
            return Err(TermsError::TooShort {
                found: file_length.min(bytes.len() as u64),
            });
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
        let candidate_term = i64::from_le_bytes(bytes[16..24].try_into().unwrap_or([0; 8]));
        let checksum = u32::from_le_bytes(bytes[24..28].try_into().unwrap_or([0; 4]));
        let candidate_torn = checksum != crc32(&bytes[16..24]);
        let entries = Self::entries(file_length);
        if entries > MAX_ENTRIES {
            return Err(TermsError::Length { found: entries });
        }
        if candidate_torn {
            return Err(TermsError::Candidate);
        }
        Ok(Self { candidate_term })
    }

    /// The whole entries a file of `file_length` bytes holds after the header.
    pub const fn entries(file_length: u64) -> u64 {
        file_length.saturating_sub(HEADER_LENGTH as u64) / ENTRY_LENGTH as u64
    }
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
    /// entry before it: the first failure by name. An empty entry parses as itself, the
    /// file's end, its other fields unread.
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

    /// Creates the file in `dir` with its header, the candidate term −1 and no entry,
    /// synced; a file already there is [`TermsError::Io`].
    pub fn create(dir: &Path) -> Result<Self, TermsError> {
        let path = Self::path_in(dir);
        let io = |what: &'static str| {
            let path = path.clone();
            move |source| TermsError::Io { what, path, source }
        };
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io("create"))?;
        let header = TermsHeader {
            candidate_term: NO_CANDIDATE_TERM,
        };
        file.write_all(&header.encode()).map_err(io("write"))?;
        file.sync_all().map_err(io("sync"))?;
        Ok(Self {
            path,
            file,
            candidate_term: NO_CANDIDATE_TERM,
            entries: Vec::new(),
        })
    }

    /// Opens the file in `dir`, reading its header and every entry up to the torn tail,
    /// which the next write overwrites; a candidate checksum that does not match reads
    /// as −1. Creates the file when there is none. The file's length is checked
    /// against the bound before a byte is read, and the entries are read 128 at a time
    /// into a fixed buffer, so that what the reader holds is bounded by [`MAX_ENTRIES`]
    /// whatever is on disk.
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
        let candidate_term = match TermsHeader::parse(&header, file_length) {
            Ok(header) => header.candidate_term,
            Err(TermsError::Candidate) => NO_CANDIDATE_TERM,
            Err(error) => return Err(error),
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
            for entry_bytes in bytes.chunks_exact(ENTRY_LENGTH) {
                let entry = match TermsEntry::parse(entry_bytes, previous.as_ref()) {
                    Ok(entry) => entry,
                    Err(TermsError::Checksum) => break 'read,
                    Err(error) => return Err(error),
                };
                if entry.state == EMPTY {
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

    /// Appends a term: its fields and their checksum, synced, then the state, synced.
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

    /// Raises the candidate term: the value, then its checksum, synced. A value below −1
    /// or below the candidate term stored is refused, since the module only raises it
    /// ("The driver"); the value stored already needs no write.
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
        let header = TermsHeader { candidate_term };
        let bytes = header.encode();
        self.write_at(16, &bytes[16..24], "store")?;
        self.write_at(24, &bytes[24..28], "store")?;
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
    fn a_torn_tail_ends_the_file_and_a_torn_candidate_reads_as_none() {
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
        // A third entry whose checksum is wrong, as a crash mid-write leaves it.
        let mut bad = TermsEntry {
            state: VALID,
            leadership_term_id: 4,
            term_base_log_position: 8192,
            recording_id: 2,
        }
        .encode();
        bad[4] ^= 0xFF;
        bytes.extend_from_slice(&bad);
        // The candidate term's checksum torn.
        bytes[24] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("written");
        let file = TermsFile::open(&dir).expect("opened");
        assert_eq!(file.terms().count(), 1);
        assert_eq!(file.candidate_term(), NO_CANDIDATE_TERM);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tail_torn_by_its_checksum_alone_ends_the_file_and_the_next_append_overwrites_it() {
        let dir = dir("torn-checksum");
        let mut file = TermsFile::create(&dir).expect("created");
        file.append(2, 0, 0).expect("appended");
        file.append(3, 4096, 1).expect("appended");
        drop(file);
        let path = TermsFile::path_in(&dir);
        let mut bytes = std::fs::read(&path).expect("read");
        // Entry 1's state is 1 and its fields whole: only its checksum fails.
        bytes[HEADER_LENGTH + ENTRY_LENGTH + 4] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("written");
        let mut file = TermsFile::open(&dir).expect("opened");
        assert_eq!(
            file.log_leadership_term_id(),
            2,
            "the torn entry is not read"
        );
        assert_eq!(file.entries.len(), 1);
        // The next append takes the torn entry's place; reopened, the file holds the
        // new term in it and nothing behind it.
        file.append(4, 8192, 2)
            .expect("appended over the torn tail");
        drop(file);
        let file = TermsFile::open(&dir).expect("opened");
        let terms: Vec<_> = file
            .terms()
            .map(|entry| {
                (
                    entry.leadership_term_id,
                    entry.term_base_log_position,
                    entry.recording_id,
                )
            })
            .collect();
        assert_eq!(terms, vec![(2, 0, 0), (4, 8192, 2)]);
        assert_eq!(file.entries.len(), 2);
        let length = std::fs::metadata(&path).expect("stat").len();
        assert_eq!(length, (HEADER_LENGTH + 2 * ENTRY_LENGTH) as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_over_the_bound_is_length_before_an_entry_is_read_and_after_a_torn_candidate() {
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
        // The candidate checksum torn too: the reader goes on past `candidate`, so the
        // file is still `length`, from the header's parse as from the open.
        grown.write_all_at(&[0xFF], 24).expect("torn");
        drop(grown);
        assert_eq!(TermsFile::open(&dir).unwrap_err().reason(), "length");
        let mut header = TermsHeader { candidate_term: 7 }.encode();
        header[24] ^= 0xFF;
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
        let mut bytes = TermsHeader { candidate_term: 3 }.encode().to_vec();
        for term in 0..300 {
            let entry = TermsEntry {
                state: VALID,
                leadership_term_id: term,
                term_base_log_position: term * 4096,
                recording_id: term,
            };
            bytes.extend_from_slice(&entry.encode());
        }
        // Entry 200, in the reader's second buffer, torn by its checksum alone.
        bytes[HEADER_LENGTH + 200 * ENTRY_LENGTH + 4] ^= 0xFF;
        std::fs::write(TermsFile::path_in(&dir), &bytes).expect("written");
        let file = TermsFile::open(&dir).expect("opened");
        assert_eq!(file.terms().count(), 200);
        assert_eq!(file.log_leadership_term_id(), 199);
        assert_eq!(file.candidate_term(), 3);
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
        let file = TermsFile::open(&dir).expect("opened");
        assert_eq!(file.candidate_term(), 5, "the refusals wrote nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
