//! The recorder's side of a recording in the log directory: [`LogDir::start`] creates
//! the slot and the files, a [`RecordingWriter`] writes blocks of committed frames into
//! segment files, keeps the time index and syncs once per second behind two positions,
//! and [`LogDir::stop`] writes the stop (`spec/durable-log.md`, "Segment files", "Time
//! index" and "The driver"; ADR 0024, ADR 0025). The clock is the caller's: every
//! method takes `now`, nanoseconds since the Unix epoch, as the driver reads it once per
//! duty cycle, so nothing here reads a clock.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::catalog::CatalogError;
use crate::dir::LogDir;
use crate::layout::{
    Descriptor, ENTRY_LENGTH, FRAME_ALIGNMENT, HEADER_LENGTH, IndexEntry, IndexHeader, LayoutError,
    Recording, index_file_name, locate, segment_file_name,
};

const DOC: &str = "docs/reference/durable-log.md#the-driver";

/// How often the recorder syncs a recording's segment and index: once per second while
/// it has unsynced bytes (ADR 0025). Nanoseconds.
pub const SYNC_INTERVAL: i64 = 1_000_000_000;

/// How often the recorder appends a time index entry while the position moves: at most
/// once per millisecond (`spec/durable-log.md`, "Time index"). Nanoseconds.
pub const INDEX_INTERVAL: i64 = 1_000_000;

/// Why a recording could not be started, written or stopped.
#[derive(Debug)]
pub enum RecorderError {
    /// The catalog refused: every slot used, an unknown recording, a transition the page
    /// does not define, or a field past its bound.
    Catalog(CatalogError),
    /// A block the writer cannot take. The recorder's scan hands whole frames of one
    /// term, which a term never lets cross a segment, so this is a bug in the caller.
    Block {
        /// The writer's position.
        position: i64,
        /// The block's length in the stream.
        length: usize,
        /// How many bytes were to be written.
        bytes: usize,
        /// What was wrong.
        reason: &'static str,
    },
    /// The operating system refused a step on a file of the recording.
    Io {
        /// The step.
        what: &'static str,
        /// The path.
        path: PathBuf,
        /// The error.
        source: io::Error,
    },
}

impl RecorderError {
    /// The reason's name: the catalog's, `block` or `io`.
    pub const fn reason(&self) -> &'static str {
        match self {
            RecorderError::Catalog(error) => error.reason(),
            RecorderError::Block { .. } => "block",
            RecorderError::Io { .. } => "io",
        }
    }
}

impl std::fmt::Display for RecorderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecorderError::Catalog(error) => write!(f, "{error}"),
            RecorderError::Block {
                position,
                length,
                bytes,
                reason,
            } => write!(
                f,
                "recorder refused (block): a block of {length} bytes with {bytes} to write at \
                 position {position} {reason}; the recorder's scan hands whole frames of one \
                 term, so this is a bug: report it with the output of `cavalry doctor`; see \
                 {DOC}"
            ),
            RecorderError::Io { what, path, source } => write!(
                f,
                "recorder refused (io): cannot {what} at {}: {source}; check the log \
                 directory's free space and permissions with `cavalry doctor`; see {DOC}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for RecorderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RecorderError::Catalog(error) => Some(error),
            RecorderError::Io { source, .. } => Some(source),
            RecorderError::Block { .. } => None,
        }
    }
}

impl From<CatalogError> for RecorderError {
    fn from(error: CatalogError) -> Self {
        RecorderError::Catalog(error)
    }
}

impl From<LayoutError> for RecorderError {
    fn from(error: LayoutError) -> Self {
        RecorderError::Catalog(CatalogError::Layout(error))
    }
}

fn io<'a>(what: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> RecorderError + 'a {
    move |source| RecorderError::Io {
        what,
        path: path.to_path_buf(),
        source,
    }
}

/// A live recording's writer: the segment file the position is in, the time index, and
/// the two positions a reader of the counters sees, what is written and what is synced.
/// It holds no buffer: a block is written from the caller's bytes with one positional
/// write, and nothing here allocates after [`LogDir::start`].
pub struct RecordingWriter {
    descriptor: Descriptor,
    dir: PathBuf,
    position: i64,
    synced_position: i64,
    segment: File,
    segment_path: PathBuf,
    segment_base: i64,
    segment_length: i64,
    index: File,
    index_path: PathBuf,
    entries: u64,
    last_index_timestamp: i64,
    last_sync_timestamp: i64,
    unsynced: bool,
}

impl std::fmt::Debug for RecordingWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingWriter")
            .field("recording_id", &self.descriptor.recording_id())
            .field("position", &self.position)
            .field("synced_position", &self.synced_position)
            .field("segment_base", &self.segment_base)
            .finish()
    }
}

impl LogDir {
    /// Starts a recording at `now`: the catalog slot in the page's order, the time
    /// index with its header and the start entry, synced, then the first segment file
    /// at its full length. Every slot used is [`CatalogError::Full`], a channel or
    /// source identity past its field is refused before any slot is written, and the
    /// recording id is the slot's. A step that fails after the slot is written leaves no
    /// writer to stop it, so `start` stops the slot itself at its start position and
    /// start timestamp, the empty recording recovery would make of it
    /// (`spec/durable-log.md`, "The driver"), and returns the step's error; if that stop
    /// fails too, the slot stays live until the next [`LogDir::open`] repairs it the
    /// same way. A step that fails at the segment file leaves the index file, header and
    /// start entry, beside the stopped, empty slot; it is harmless, since a recording id
    /// is never reused, and it is kept so that nothing is removed on an error path.
    pub fn start(
        &mut self,
        recording: &Recording<'_>,
        now: i64,
    ) -> Result<RecordingWriter, RecorderError> {
        // The fields are checked before the slot is written: a channel past its field
        // refuses here, with nothing on disk.
        Descriptor::new(recording)?;
        let id = self.catalog_mut().add(recording)?;
        self.start_files(id, now).inspect_err(|_| {
            // The step's error is the one the caller needs; a failed stop here is
            // repaired by recovery at the next open.
            let _ =
                self.catalog_mut()
                    .stop(id, recording.start_position, recording.start_timestamp);
        })
    }

    fn start_files(&mut self, id: i64, now: i64) -> Result<RecordingWriter, RecorderError> {
        let descriptor = self
            .catalog()
            .get(id)?
            .ok_or(CatalogError::Unknown { recording_id: id })?;
        let dir = self.dir().to_path_buf();
        let index_path = dir.join(index_file_name(id));
        let index = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&index_path)
            .map_err(io("create", &index_path))?;
        let header = IndexHeader::new(id)?.encode();
        index
            .write_all_at(&header[8..], 8)
            .map_err(io("write the header of", &index_path))?;
        index.sync_all().map_err(io("sync", &index_path))?;
        index
            .write_all_at(&header[..8], 0)
            .map_err(io("write the magic of", &index_path))?;
        let start = descriptor.start_position();
        let first = IndexEntry {
            timestamp: now,
            position: start,
        };
        index
            .write_all_at(&first.encode(), HEADER_LENGTH as u64)
            .map_err(io("write the first entry of", &index_path))?;
        index.sync_all().map_err(io("sync", &index_path))?;
        let located = locate(&descriptor, start)?;
        let segment_length = i64::from(descriptor.segment_file_length());
        let segment_path = dir.join(&located.segment_file);
        let segment = open_segment(&segment_path, segment_length)?;
        Ok(RecordingWriter {
            descriptor,
            dir,
            position: start,
            synced_position: start,
            segment,
            segment_path,
            segment_base: located.segment_base_position,
            segment_length,
            index,
            index_path,
            entries: 1,
            last_index_timestamp: now,
            last_sync_timestamp: now,
            unsynced: false,
        })
    }

    /// Stops a recording at `now`: the index gets its stop entry, the segment and the
    /// index are synced, a segment file wholly past the stop, one the writer had just
    /// rolled into, is removed and the removal synced, and then the catalog's stop
    /// fields are written and synced with the stop entry's timestamp, which never goes
    /// below the last entry's. The removal comes first so that a crash between the two
    /// leaves a live slot, which recovery scans and repairs, never a stopped slot with
    /// a segment past its stop that nothing revisits. Returns the stopped descriptor.
    pub fn stop(
        &mut self,
        mut writer: RecordingWriter,
        now: i64,
    ) -> Result<Descriptor, RecorderError> {
        writer.append_entry(now)?;
        writer.sync_files()?;
        let id = writer.descriptor.recording_id();
        if writer.segment_base >= writer.position {
            fs::remove_file(&writer.segment_path).map_err(io("remove", &writer.segment_path))?;
            File::open(&writer.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(io("sync", &writer.dir))?;
        }
        // The stop timestamp is the stop entry's: never before the last entry's, so a
        // clock that stepped back cannot put the stop before the start.
        self.catalog_mut()
            .stop(id, writer.position, writer.last_index_timestamp)?;
        Ok(self
            .catalog()
            .get(id)?
            .ok_or(CatalogError::Unknown { recording_id: id })?)
    }
}

/// Creates a segment file at its full length, synced, so that its length is durable
/// before a byte is written into it. A file it created and could not extend or sync is
/// removed again, so that a retry, after the operator frees space, can create it.
fn open_segment(path: &Path, length: i64) -> Result<File, RecorderError> {
    let segment = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io("create", path))?;
    let extended = segment
        .set_len(length as u64)
        .map_err(io("extend", path))
        .and_then(|()| segment.sync_all().map_err(io("sync", path)));
    if let Err(error) = extended {
        // The extend's error is the one the operator needs; a file left behind makes
        // the retry's create refuse, which names the path.
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(segment)
}

impl RecordingWriter {
    /// The recording id: the catalog slot.
    pub fn recording_id(&self) -> i64 {
        self.descriptor.recording_id()
    }

    /// The descriptor as the catalog holds it, live.
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// The position after the last block written: what the operating system holds.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// The position the last sync confirmed: what survives a power loss.
    pub fn synced_position(&self) -> i64 {
        self.synced_position
    }

    /// Writes one block at the position: `bytes`, the frames with their headers as the
    /// scan hands them, with one positional write into the segment, and the position
    /// advances by `length`, the block's length in the stream, which is `bytes.len()`
    /// for a block of data frames and the padding frame's aligned length for a padding
    /// block, whose bytes are its 32-byte header alone. Appends a time index entry when
    /// a millisecond has passed since the last, with a timestamp that never goes below
    /// the last entry's, and rolls into the next segment file when this one is full,
    /// syncing the one it closes. A block that is empty, not a multiple of 32, longer
    /// in bytes than in length, or that would cross the segment's end is
    /// [`RecorderError::Block`], and nothing moves.
    ///
    /// On an error, the block was taken exactly when [`position`](Self::position) moved:
    /// the caller compares it before and after the call, hands the block again when it
    /// did not move, and hands the next one when it did. `write` writes at the position,
    /// never at a position the caller names, so a block dropped that was not taken would
    /// shift every block after it. An [`RecorderError::Io`] error can come before the
    /// block is written or after, from the index entry or the roll into the next
    /// segment, and its step does not say which. A roll that failed, a full disk when the
    /// next segment is created, is retried at the start of the next `write`, which
    /// returns the roll's `Io` error again, with nothing moved, until the roll succeeds.
    pub fn write(&mut self, bytes: &[u8], length: usize, now: i64) -> Result<(), RecorderError> {
        if self.position == self.segment_base + self.segment_length {
            self.roll()?;
        }
        let refuse = |reason| RecorderError::Block {
            position: self.position,
            length,
            bytes: bytes.len(),
            reason,
        };
        if length == 0 {
            return Err(refuse("is empty"));
        }
        let alignment = FRAME_ALIGNMENT as usize;
        if !length.is_multiple_of(alignment) || !bytes.len().is_multiple_of(alignment) {
            return Err(refuse("is not a multiple of 32"));
        }
        if bytes.len() > length {
            return Err(refuse("has more bytes than its length"));
        }
        let end = self.position + length as i64;
        if end > self.segment_base + self.segment_length {
            return Err(refuse("would cross the end of its segment file"));
        }
        self.segment
            .write_all_at(bytes, (self.position - self.segment_base) as u64)
            .map_err(io("write a block of", &self.segment_path))?;
        self.position = end;
        self.unsynced = true;
        if now - self.last_index_timestamp >= INDEX_INTERVAL {
            self.append_entry(now)?;
        }
        if self.position == self.segment_base + self.segment_length {
            self.roll()?;
        }
        Ok(())
    }

    /// Syncs the segment and the index when a second has passed since the last sync and
    /// there are unsynced bytes, and moves the synced position up to the position.
    /// Returns whether it synced.
    pub fn sync(&mut self, now: i64) -> Result<bool, RecorderError> {
        if !self.unsynced || now - self.last_sync_timestamp < SYNC_INTERVAL {
            return Ok(false);
        }
        self.sync_files()?;
        self.last_sync_timestamp = now;
        Ok(true)
    }

    fn sync_files(&mut self) -> Result<(), RecorderError> {
        self.segment
            .sync_data()
            .map_err(io("sync", &self.segment_path))?;
        self.index
            .sync_data()
            .map_err(io("sync", &self.index_path))?;
        self.synced_position = self.position;
        self.unsynced = false;
        Ok(())
    }

    fn append_entry(&mut self, now: i64) -> Result<(), RecorderError> {
        // Timestamps never decrease from one entry to the next (`spec/durable-log.md`,
        // "Time index"): a clock that stepped back is written as the last entry's.
        let timestamp = now.max(self.last_index_timestamp);
        let entry = IndexEntry {
            timestamp,
            position: self.position,
        };
        self.index
            .write_all_at(
                &entry.encode(),
                HEADER_LENGTH as u64 + self.entries * ENTRY_LENGTH as u64,
            )
            .map_err(io("write an entry of", &self.index_path))?;
        self.entries += 1;
        self.last_index_timestamp = timestamp;
        self.unsynced = true;
        Ok(())
    }

    fn roll(&mut self) -> Result<(), RecorderError> {
        self.segment
            .sync_data()
            .map_err(io("sync", &self.segment_path))?;
        let base = self.segment_base + self.segment_length;
        let path = self
            .dir
            .join(segment_file_name(self.descriptor.recording_id(), base));
        self.segment = open_segment(&path, self.segment_length)?;
        self.segment_path = path;
        self.segment_base = base;
        Ok(())
    }
}
