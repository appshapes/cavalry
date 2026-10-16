//! The catalog on disk: the driver's writer and any process's reader
//! (`spec/durable-log.md`, "Catalog header", "Descriptor" and "The driver"; ADR 0024).
//! Positional reads and writes through `std::fs`, no mapping and no `unsafe`; off the
//! message path, so a write may be a syscall and an fsync.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::layout::{
    CATALOG_FILE_NAME, CatalogHeader, DESCRIPTOR_LENGTH, Descriptor, HEADER_LENGTH, LayoutError,
    MAX_SLOTS, Recording, State, crc32,
};

const DOC: &str = "docs/reference/durable-log.md#catalog-header";
/// How long a reader retries a descriptor whose checksum does not match before it
/// reports `checksum`: the driver may be between the two writes of a stop.
const RETRY: Duration = Duration::from_millis(100);
const RETRY_STEP: Duration = Duration::from_millis(1);

/// Why the catalog could not be used.
#[derive(Debug)]
pub enum CatalogError {
    /// The bytes failed a check of the page.
    Layout(LayoutError),
    /// Every slot is used: move or archive the log directory.
    Full,
    /// No such slot, or a slot that holds no recording.
    Unknown {
        /// The id asked for.
        recording_id: i64,
    },
    /// A write the page does not define: a stop on a recording that is not live, a
    /// delete of one that is live or already deleted, an invalidation of one already
    /// invalid. Nothing is written.
    Transition {
        /// The recording.
        recording_id: i64,
        /// The state it is in.
        from: State,
        /// What the write needed.
        needed: &'static str,
    },
    /// The descriptor given to [`CatalogWriter::rewrite`] is not the one its slot holds:
    /// its creation fields are another recording's.
    Mismatch {
        /// The slot.
        recording_id: i64,
    },
    /// The operating system refused a step.
    Io {
        /// The step.
        what: &'static str,
        /// The path.
        path: PathBuf,
        /// The error.
        source: io::Error,
    },
}

impl CatalogError {
    /// The reason's name: a layout check's, or `full`, `unknown`, `mismatch`, `io`.
    pub const fn reason(&self) -> &'static str {
        match self {
            CatalogError::Layout(error) => error.reason(),
            CatalogError::Full => "full",
            CatalogError::Unknown { .. } => "unknown",
            CatalogError::Mismatch { .. } => "mismatch",
            CatalogError::Transition { .. } => "transition",
            CatalogError::Io { .. } => "io",
        }
    }
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Layout(error) => write!(f, "{error}"),
            CatalogError::Full => write!(
                f,
                "catalog full: every one of its {MAX_SLOTS} slots is used; move or archive the \
                 log directory, and a new catalog is created in its place when the directory \
                 is next opened; see {DOC}"
            ),
            CatalogError::Unknown { recording_id } => write!(
                f,
                "unknown recording {recording_id}: no slot holds it; `cavalry doctor` lists the \
                 recordings; see {DOC}"
            ),
            CatalogError::Transition {
                recording_id,
                from,
                needed,
            } => write!(
                f,
                "catalog refused (transition): recording {recording_id} is {from:?}, and this write \
                 needs {needed}; a stop is written once when a live recording stops, a delete \
                 once when a stopped one is purged, and recovery alone marks damage; nothing was \
                 written; see {DOC}"
            ),
            CatalogError::Mismatch { recording_id } => write!(
                f,
                "catalog refused (mismatch): the descriptor given for slot {recording_id} is not \
                 the one the slot holds, so its stop fields were not written; read the slot \
                 with `get` and stop that descriptor; see {DOC}"
            ),
            CatalogError::Io { what, path, source } => write!(
                f,
                "catalog refused (io): cannot {what} at {}: {source}; see {DOC}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CatalogError::Layout(error) => Some(error),
            CatalogError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<LayoutError> for CatalogError {
    fn from(error: LayoutError) -> Self {
        CatalogError::Layout(error)
    }
}

/// The catalog as the driver holds it: the one writer.
pub struct CatalogWriter {
    file: File,
    path: PathBuf,
    slots: u64,
}

/// The catalog as any process reads it.
pub struct CatalogReader {
    file: File,
    path: PathBuf,
}

fn io<'a>(what: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> CatalogError + 'a {
    move |source| CatalogError::Io {
        what,
        path: path.to_path_buf(),
        source,
    }
}

fn file_length(file: &File, path: &Path) -> Result<u64, CatalogError> {
    Ok(file.metadata().map_err(io("stat", path))?.len())
}

fn read_header(file: &File, path: &Path) -> Result<CatalogHeader, CatalogError> {
    let length = file_length(file, path)?;
    let mut bytes = [0u8; HEADER_LENGTH];
    if length >= HEADER_LENGTH as u64 {
        file.read_exact_at(&mut bytes, 0)
            .map_err(io("read the header of", path))?;
    }
    Ok(CatalogHeader::parse(&bytes, length)?)
}

fn read_slot_bytes(
    file: &File,
    path: &Path,
    slot: u64,
) -> Result<[u8; DESCRIPTOR_LENGTH], CatalogError> {
    let mut bytes = [0u8; DESCRIPTOR_LENGTH];
    file.read_exact_at(&mut bytes, CatalogHeader::slot_offset(slot))
        .map_err(io("read a slot of", path))?;
    Ok(bytes)
}

/// Reads a slot as a reader does: a stop checksum that does not match is retried for
/// [`RETRY`], since the driver may be between the two writes of a stop.
fn read_slot(file: &File, path: &Path, slot: u64) -> Result<Option<Descriptor>, CatalogError> {
    let mut waited = Duration::ZERO;
    loop {
        let bytes = read_slot_bytes(file, path, slot)?;
        match Descriptor::parse(&bytes, slot as i64) {
            Err(LayoutError::StopChecksum) if waited < RETRY => {
                std::thread::sleep(RETRY_STEP);
                waited += RETRY_STEP;
            }
            other => return Ok(other?),
        }
    }
}

/// Whether the catalog's header was torn while the driver created it: the file is
/// shorter than the header, or its magic is zero and it holds no whole slot, which the
/// creation order allows since the header is synced before a slot is added
/// (`spec/durable-log.md`, "The driver").
fn header_is_torn(file: &File, path: &Path, length: u64) -> Result<bool, CatalogError> {
    if length < HEADER_LENGTH as u64 {
        return Ok(true);
    }
    let mut magic = [0u8; 8];
    file.read_exact_at(&mut magic, 0)
        .map_err(io("read the header of", path))?;
    Ok(magic == [0u8; 8] && CatalogHeader::slots(length) == 0)
}

/// Writes the header in the page's order: every field and a sync, then the magic and a
/// sync, so that a reader sees `not-ready` and never a part-written header.
fn write_header(file: &File, path: &Path, now: i64) -> Result<(), CatalogError> {
    let encoded = CatalogHeader::new(now)?.encode();
    file.set_len(0)
        .map_err(io("truncate the header of", path))?;
    file.write_all_at(&encoded[8..], 8)
        .map_err(io("write the header of", path))?;
    file.sync_all().map_err(io("sync", path))?;
    file.write_all_at(&encoded[..8], 0)
        .map_err(io("write the magic of", path))?;
    file.sync_all().map_err(io("sync", path))?;
    Ok(())
}

impl CatalogWriter {
    /// Creates the catalog in `dir`, or opens the one there. A catalog whose header was
    /// torn while it was created is written again, and an empty last slot, a creation
    /// torn by a crash, is dropped from the file, so the next [`add`](Self::add) takes
    /// its id and a later open finds nothing to drop. The
    /// driver that owns the directory is its one writer; how it keeps a second driver
    /// out is the driver's (card 93), not this type's.
    pub fn open(dir: &Path, now: i64) -> Result<Self, CatalogError> {
        let path = dir.join(CATALOG_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io("open", &path))?;
        let length = file_length(&file, &path)?;
        if header_is_torn(&file, &path, length)? {
            write_header(&file, &path, now)?;
        } else {
            read_header(&file, &path)?;
        }
        let length = file_length(&file, &path)?;
        let mut slots = CatalogHeader::slots(length);
        let whole = CatalogHeader::slot_offset(slots);
        if length > whole {
            // Bytes past the last whole slot, which a crash while the file grew leaves:
            // the page says the driver truncates them at start-up, and the truncation is
            // synced so that a crash before the next sync cannot bring them back.
            file.set_len(whole).map_err(io("truncate", &path))?;
            file.sync_all().map_err(io("sync", &path))?;
        }
        if slots > 0 && read_slot_bytes(&file, &path, slots - 1)?[..4] == [0u8; 4] {
            // An empty last slot, a creation torn by a crash: the page says the driver
            // drops it, so the file loses it and the next recording id is the number of
            // whole slots left, which a later start finds nothing to drop again.
            slots -= 1;
            file.set_len(CatalogHeader::slot_offset(slots))
                .map_err(io("drop the empty last slot of", &path))?;
            file.sync_all().map_err(io("sync", &path))?;
        }
        Ok(CatalogWriter { file, path, slots })
    }

    /// How many slots the catalog holds: whole slots, after an empty last slot left by a
    /// torn creation was dropped at open.
    pub const fn slots(&self) -> u64 {
        self.slots
    }

    /// The descriptor in slot `recording_id`, `None` when the slot is empty. The writer
    /// is the only writer, so it is never in the middle of its own stop and does not
    /// retry a stop checksum.
    pub fn get(&self, recording_id: i64) -> Result<Option<Descriptor>, CatalogError> {
        let slot = self.slot(recording_id)?;
        let bytes = read_slot_bytes(&self.file, &self.path, slot)?;
        Ok(Descriptor::parse(&bytes, recording_id)?)
    }

    /// The catalog's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A slot's bytes as they are, for the recovery pass, which reads a slot whose stop
    /// or creation checksum may fail.
    pub(crate) fn slot_bytes(&self, slot: u64) -> Result<[u8; DESCRIPTOR_LENGTH], CatalogError> {
        if slot >= self.slots {
            return Err(CatalogError::Unknown {
                recording_id: i64::try_from(slot).unwrap_or(i64::MAX),
            });
        }
        read_slot_bytes(&self.file, &self.path, slot)
    }

    /// Recovery's write of state 2 on a slot it found damaged, whatever the slot holds:
    /// an empty slot before the last, a creation checksum that fails, or a recording
    /// whose files cannot repair it. [`invalidate`](Self::invalidate) reads the slot
    /// first and refuses an empty one, which is right for every caller but recovery.
    pub(crate) fn mark_invalid(&mut self, slot: u64) -> Result<(), CatalogError> {
        if slot >= self.slots {
            return Err(CatalogError::Unknown {
                recording_id: i64::try_from(slot).unwrap_or(i64::MAX),
            });
        }
        self.write_state(slot, State::Invalid)
    }

    fn slot(&self, recording_id: i64) -> Result<u64, CatalogError> {
        match u64::try_from(recording_id) {
            Ok(slot) if slot < self.slots => Ok(slot),
            _ => Err(CatalogError::Unknown { recording_id }),
        }
    }

    /// Adds a recording in the next slot: the file grows by one zeroed slot, the body
    /// and the checksum are written, the state last; returns the recording id.
    pub fn add(&mut self, recording: &Recording<'_>) -> Result<i64, CatalogError> {
        if self.slots >= MAX_SLOTS {
            return Err(CatalogError::Full);
        }
        let id = self.slots as i64;
        let descriptor = Descriptor::new(&Recording {
            recording_id: id,
            ..recording.clone()
        })?;
        let offset = CatalogHeader::slot_offset(self.slots);
        let encoded = descriptor.encode();
        self.file
            .write_all_at(&[0u8; DESCRIPTOR_LENGTH], offset)
            .map_err(io("grow", &self.path))?;
        self.file
            .write_all_at(&encoded[24..], offset + 24)
            .map_err(io("write the creation fields of", &self.path))?;
        self.file
            .write_all_at(&encoded[4..24], offset + 4)
            .map_err(io("write the stop fields of", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        self.file
            .write_all_at(&encoded[..4], offset)
            .map_err(io("commit a slot of", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        self.slots += 1;
        Ok(id)
    }

    /// Writes the stop fields of a live recording, then the checksum: the one stop the
    /// page writes. A recording that is stopped or deleted is `transition`, and nothing
    /// is written; recovery rebuilds a torn stop with [`rewrite`](Self::rewrite).
    pub fn stop(
        &mut self,
        recording_id: i64,
        stop_position: i64,
        stop_timestamp: i64,
    ) -> Result<(), CatalogError> {
        let descriptor = self
            .get(recording_id)?
            .ok_or(CatalogError::Unknown { recording_id })?;
        if descriptor.state() != State::Valid || !descriptor.is_live() {
            return Err(CatalogError::Transition {
                recording_id,
                from: descriptor.state(),
                needed: "a live recording",
            });
        }
        let descriptor = descriptor.stopped(stop_position, stop_timestamp)?;
        self.rewrite(self.slot(recording_id)?, &descriptor)
    }

    // The delete the page defines, state 3 on a stopped recording, arrives with card
    // 95's purge, with its counter and its caller in the driver: a transition the driver
    // cannot yet perform is not written here, so that every catalog transition on master
    // is one the driver performs and counts (CLAUDE.md, invariant 4).

    /// Writes state 2 on a slot recovery found damaged: valid or deleted, whatever its
    /// checksums say, since damage is why. An empty slot is `unknown`, an invalid one
    /// is `transition`, and nothing is written.
    pub fn invalidate(&mut self, recording_id: i64) -> Result<(), CatalogError> {
        let slot = self.slot(recording_id)?;
        let found = read_slot_bytes(&self.file, &self.path, slot)?;
        let state = i32::from_le_bytes([found[0], found[1], found[2], found[3]]);
        match State::from_i32(state) {
            Some(State::Valid | State::Deleted) => self.write_state(slot, State::Invalid),
            Some(State::Empty) => Err(CatalogError::Unknown { recording_id }),
            Some(State::Invalid) => Err(CatalogError::Transition {
                recording_id,
                from: State::Invalid,
                needed: "a valid or deleted recording",
            }),
            None => Err(LayoutError::State { found: state }.into()),
        }
    }

    fn write_state(&mut self, slot: u64, state: State) -> Result<(), CatalogError> {
        let offset = CatalogHeader::slot_offset(slot);
        self.file
            .write_all_at(&(state as i32).to_le_bytes(), offset)
            .map_err(io("write a state of", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        Ok(())
    }

    /// Writes a slot's stop fields and then their checksum; the creation fields and the
    /// state are not touched, so recovery never re-signs a creation field. The slot's
    /// stop checksum may fail, since rebuilding a torn stop is what this is for, but the
    /// rest must hold: a slot past the last is `unknown`, an empty one is `unknown`, a
    /// state-2 one is `invalid`, a creation checksum that fails is `checksum`, a
    /// descriptor whose id is not `slot` is `recording-id`, and one whose creation
    /// fields are not the slot's is `mismatch`. Nothing is written when any fails.
    pub fn rewrite(&mut self, slot: u64, descriptor: &Descriptor) -> Result<(), CatalogError> {
        let recording_id = i64::try_from(slot).unwrap_or(i64::MAX);
        if slot >= self.slots {
            return Err(CatalogError::Unknown { recording_id });
        }
        let found = read_slot_bytes(&self.file, &self.path, slot)?;
        let state = i32::from_le_bytes([found[0], found[1], found[2], found[3]]);
        match State::from_i32(state) {
            Some(State::Valid | State::Deleted) => {}
            Some(State::Empty) => return Err(CatalogError::Unknown { recording_id }),
            Some(State::Invalid) => return Err(LayoutError::Invalid.into()),
            None => return Err(LayoutError::State { found: state }.into()),
        }
        let creation_checksum = u32::from_le_bytes([found[24], found[25], found[26], found[27]]);
        if creation_checksum != crc32(&found[28..]) {
            return Err(LayoutError::CreationChecksum.into());
        }
        if descriptor.recording_id() != recording_id {
            return Err(LayoutError::RecordingId {
                found: descriptor.recording_id(),
            }
            .into());
        }
        let offset = CatalogHeader::slot_offset(slot);
        let encoded = descriptor.encode();
        // Bytes 32 to 472 are the creation fields; the reserved bytes around them are
        // never read, so another writer's values there are not a mismatch.
        if encoded[32..472] != found[32..472] {
            return Err(CatalogError::Mismatch { recording_id });
        }
        self.file
            .write_all_at(&encoded[8..24], offset + 8)
            .map_err(io("write the stop fields of", &self.path))?;
        self.file
            .write_all_at(&encoded[4..8], offset + 4)
            .map_err(io("write the stop checksum of", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        Ok(())
    }
}

impl CatalogReader {
    /// Opens the catalog in `dir` for reading.
    pub fn open(dir: &Path) -> Result<Self, CatalogError> {
        let path = dir.join(CATALOG_FILE_NAME);
        let file = File::open(&path).map_err(io("open", &path))?;
        read_header(&file, &path)?;
        Ok(CatalogReader { file, path })
    }

    /// How many slots the catalog holds now.
    pub fn slots(&self) -> Result<u64, CatalogError> {
        read_header(&self.file, &self.path)?;
        Ok(CatalogHeader::slots(file_length(&self.file, &self.path)?))
    }

    /// The descriptor in slot `recording_id`, `None` when the slot is empty.
    pub fn get(&self, recording_id: i64) -> Result<Option<Descriptor>, CatalogError> {
        if recording_id < 0 || recording_id as u64 >= self.slots()? {
            return Err(CatalogError::Unknown { recording_id });
        }
        read_slot(&self.file, &self.path, recording_id as u64)
    }
}
