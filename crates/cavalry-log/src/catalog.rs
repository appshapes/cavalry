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
    MAX_SLOTS, Recording, State,
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
    /// Every slot is used: start a new log directory.
    Full,
    /// No such slot.
    Unknown {
        /// The id asked for.
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
    /// The reason's name: a layout check's, or `full`, `unknown`, `io`.
    pub const fn reason(&self) -> &'static str {
        match self {
            CatalogError::Layout(error) => error.reason(),
            CatalogError::Full => "full",
            CatalogError::Unknown { .. } => "unknown",
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
                "catalog full: every one of its {MAX_SLOTS} slots is used; start a new log \
                 directory with CAVALRY_LOG_DIR and record there; see {DOC}"
            ),
            CatalogError::Unknown { recording_id } => write!(
                f,
                "unknown recording {recording_id}: no slot holds it; `cavalry doctor` lists the \
                 recordings; see {DOC}"
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

fn read_slot(file: &File, path: &Path, slot: u64) -> Result<Option<Descriptor>, CatalogError> {
    let mut bytes = [0u8; DESCRIPTOR_LENGTH];
    file.read_exact_at(&mut bytes, CatalogHeader::slot_offset(slot))
        .map_err(io("read a slot of", path))?;
    let mut waited = Duration::ZERO;
    loop {
        match Descriptor::parse(&bytes, slot as i64) {
            Err(LayoutError::StopChecksum) if waited < RETRY => {
                std::thread::sleep(RETRY_STEP);
                waited += RETRY_STEP;
                file.read_exact_at(&mut bytes, CatalogHeader::slot_offset(slot))
                    .map_err(io("read a slot of", path))?;
            }
            other => return Ok(other?),
        }
    }
}

impl CatalogWriter {
    /// Creates the catalog in `dir`, or opens the one there. The driver that owns the
    /// directory is its one writer; how it keeps a second driver out is the driver's
    /// (card 93), not this type's.
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
        if length == 0 {
            let header = CatalogHeader::new(now)?;
            file.write_all_at(&header.encode(), 0)
                .map_err(io("write the header of", &path))?;
            file.sync_all().map_err(io("sync", &path))?;
        } else {
            read_header(&file, &path)?;
        }
        let slots = CatalogHeader::slots(file_length(&file, &path)?);
        Ok(CatalogWriter { file, path, slots })
    }

    /// How many slots the catalog holds, used or empty.
    pub const fn slots(&self) -> u64 {
        self.slots
    }

    /// The descriptor in slot `recording_id`, `None` when the slot is empty.
    pub fn get(&self, recording_id: i64) -> Result<Option<Descriptor>, CatalogError> {
        if recording_id < 0 || recording_id as u64 >= self.slots {
            return Err(CatalogError::Unknown { recording_id });
        }
        read_slot(&self.file, &self.path, recording_id as u64)
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

    /// Writes the stop fields of recording `recording_id`, then the checksum.
    pub fn stop(
        &mut self,
        recording_id: i64,
        stop_position: i64,
        stop_timestamp: i64,
    ) -> Result<(), CatalogError> {
        let descriptor = self
            .get(recording_id)?
            .ok_or(CatalogError::Unknown { recording_id })?
            .stopped(stop_position, stop_timestamp)?;
        self.rewrite(recording_id as u64, &descriptor)
    }

    /// Writes the state of recording `recording_id`.
    pub fn set_state(&mut self, recording_id: i64, state: State) -> Result<(), CatalogError> {
        let descriptor = self
            .get(recording_id)?
            .ok_or(CatalogError::Unknown { recording_id })?
            .with_state(state);
        let offset = CatalogHeader::slot_offset(recording_id as u64);
        self.file
            .write_all_at(&descriptor.encode()[..4], offset)
            .map_err(io("write a state of", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        Ok(())
    }

    /// Writes a slot's stop fields and then their checksum; the creation fields and the
    /// state are not touched, so recovery never re-signs a creation field.
    pub fn rewrite(&mut self, slot: u64, descriptor: &Descriptor) -> Result<(), CatalogError> {
        let offset = CatalogHeader::slot_offset(slot);
        let encoded = descriptor.encode();
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
