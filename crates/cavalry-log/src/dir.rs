//! The log directory as the driver holds it: one writer, kept so by an advisory lock
//! on the catalog, and the recovery pass of `spec/durable-log.md`, "The driver", run
//! when the directory is opened and before any recording is added (ADR 0024, ADR 0025).

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use cavalry_proto::frame::{DataFrameHeader, HEADER_LENGTH as FRAME_HEADER_LENGTH};

use crate::catalog::{CatalogError, CatalogWriter};
use crate::layout::{
    CATALOG_FILE_NAME, DESCRIPTOR_LENGTH, Descriptor, ENTRY_LENGTH, FRAME_ALIGNMENT, HEADER_LENGTH,
    INDEX_SUFFIX, IndexEntry, IndexHeader, LayoutError, Located, SEGMENT_SUFFIX, State,
    index_file_name, locate,
};

const DOC: &str = "docs/reference/durable-log.md#the-driver";

/// Why the directory could not be opened or repaired.
#[derive(Debug)]
pub enum DirError {
    /// Another driver holds the directory: its lock on the catalog is taken.
    Locked {
        /// The directory.
        dir: PathBuf,
    },
    /// The catalog refused a step.
    Catalog(CatalogError),
    /// The operating system refused a step on a file of the directory.
    Io {
        /// The step.
        what: &'static str,
        /// The path.
        path: PathBuf,
        /// The error.
        source: io::Error,
    },
}

impl DirError {
    /// The reason's name: `locked`, `io`, or the catalog's.
    pub const fn reason(&self) -> &'static str {
        match self {
            DirError::Locked { .. } => "locked",
            DirError::Catalog(error) => error.reason(),
            DirError::Io { .. } => "io",
        }
    }
}

impl std::fmt::Display for DirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirError::Locked { dir } => write!(
                f,
                "log directory locked: another driver holds {}; stop it, or start this driver \
                 with another directory, `--log-dir`; see {DOC}",
                dir.display()
            ),
            DirError::Catalog(error) => write!(f, "{error}"),
            DirError::Io { what, path, source } => write!(
                f,
                "log directory refused (io): cannot {what} at {}: {source}; check the \
                 directory's permissions and free space with `cavalry doctor`; see {DOC}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for DirError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DirError::Catalog(error) => Some(error),
            DirError::Io { source, .. } => Some(source),
            DirError::Locked { .. } => None,
        }
    }
}

impl From<CatalogError> for DirError {
    fn from(error: CatalogError) -> Self {
        DirError::Catalog(error)
    }
}

impl From<LayoutError> for DirError {
    fn from(error: LayoutError) -> Self {
        DirError::Catalog(CatalogError::Layout(error))
    }
}

fn io<'a>(what: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> DirError + 'a {
    move |source| DirError::Io {
        what,
        path: path.to_path_buf(),
        source,
    }
}

/// What the recovery pass did when the directory was opened. Every field is a state
/// transition the driver counts (`counters.toml`; ADR 0023, card 93).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recovery {
    /// The catalog held bytes past its last whole slot, left by a crash while it grew,
    /// and was truncated to whole slots.
    pub truncated: bool,
    /// The catalog's last slot was empty, a creation torn by a crash, and the next
    /// recording takes it.
    pub dropped_empty_slot: bool,
    /// Recordings live at a crash, or whose stop was torn, whose stop fields were rebuilt
    /// from their files.
    pub recovered: u64,
    /// Slots set to state 2: an empty slot before the last, a creation checksum that
    /// fails, a state the page does not name, or files that cannot repair the recording.
    pub invalidated: u64,
    /// Time indexes written again with their first entry, given a first entry, or
    /// truncated to whole entries in order.
    pub index_rewritten: u64,
    /// Files removed: segments wholly past a rebuilt stop, and a deleted recording's
    /// files found still present.
    pub files_removed: u64,
}

/// A log directory held by its one writer, the driver (`spec/durable-log.md`, "The
/// driver"). Opening it takes an exclusive advisory lock on the catalog file, which the
/// operating system drops when the holder dies, so a crash never leaves a stale lock
/// and a second driver on the same directory is refused with `locked` (ADR 0025).
pub struct LogDir {
    dir: PathBuf,
    catalog: CatalogWriter,
    _lock: File,
}

impl std::fmt::Debug for LogDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogDir")
            .field("dir", &self.dir)
            .field("slots", &self.catalog.slots())
            .finish()
    }
}

impl LogDir {
    /// Opens `dir`, creating it and its catalog when they do not exist, takes the lock,
    /// and runs the recovery pass with `now` as the catalog's creation timestamp when it
    /// is created. Another holder of the lock is [`DirError::Locked`].
    pub fn open(dir: &Path, now: i64) -> Result<(Self, Recovery), DirError> {
        fs::create_dir_all(dir).map_err(io("create", dir))?;
        let catalog_path = dir.join(CATALOG_FILE_NAME);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&catalog_path)
            .map_err(io("open", &catalog_path))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(DirError::Locked {
                    dir: dir.to_path_buf(),
                });
            }
            Err(TryLockError::Error(source)) => {
                return Err(DirError::Io {
                    what: "lock",
                    path: catalog_path,
                    source,
                });
            }
        }
        let before = lock.metadata().map_err(io("stat", &catalog_path))?.len();
        let mut catalog = CatalogWriter::open(dir, now)?;
        let recovery = recover(dir, &mut catalog, before)?;
        Ok((
            Self {
                dir: dir.to_path_buf(),
                catalog,
                _lock: lock,
            },
            recovery,
        ))
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The catalog, held open for the directory's life.
    pub fn catalog(&self) -> &CatalogWriter {
        &self.catalog
    }

    /// The catalog, to write.
    pub fn catalog_mut(&mut self) -> &mut CatalogWriter {
        &mut self.catalog
    }
}

/// One segment file of a recording, as found in the directory.
#[derive(Debug, Clone)]
struct Segment {
    base: i64,
    path: PathBuf,
    length: u64,
}

/// The recovery pass, after [`CatalogWriter::open`] has written a torn header again,
/// truncated the file to whole slots and left an empty last slot uncounted.
fn recover(dir: &Path, catalog: &mut CatalogWriter, before: u64) -> Result<Recovery, DirError> {
    let mut recovery = Recovery::default();
    let header = HEADER_LENGTH as u64;
    let descriptor = DESCRIPTOR_LENGTH as u64;
    recovery.truncated = before > header && !(before - header).is_multiple_of(descriptor);
    let after = fs::metadata(catalog.path())
        .map_err(io("stat", catalog.path()))?
        .len();
    recovery.dropped_empty_slot =
        after >= header && (after - header) / descriptor > catalog.slots();
    for slot in 0..catalog.slots() {
        let bytes = catalog.slot_bytes(slot)?;
        let state = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        match State::from_i32(state) {
            Some(State::Valid | State::Deleted) => {}
            Some(State::Invalid) => continue,
            // An empty slot before the last cannot come from one writer, and a state the
            // page does not name is damage too.
            Some(State::Empty) | None => {
                catalog.mark_invalid(slot)?;
                recovery.invalidated += 1;
                continue;
            }
        }
        let id = i64::try_from(slot).unwrap_or(i64::MAX);
        let (found, torn) = match Descriptor::parse_for_recovery(&bytes, id) {
            Ok(Some(parsed)) => parsed,
            // A creation checksum or a creation field that fails: damage the files
            // cannot repair, and recovery never writes a creation field.
            Ok(None) | Err(_) => {
                catalog.mark_invalid(slot)?;
                recovery.invalidated += 1;
                continue;
            }
        };
        if found.state() == State::Deleted {
            // A deleted recording whose files are still present loses them again.
            recovery.files_removed += remove_files(dir, id, i64::MIN)?;
            continue;
        }
        if !torn && !found.is_live() {
            continue;
        }
        rebuild(dir, catalog, slot, &found, &mut recovery)?;
    }
    Ok(recovery)
}

/// Rebuilds the stop fields of a recording live at a crash or whose stop was torn, from
/// its files, in the page's order: the index first, then an empty recording, then the
/// scan's start, then the scan; only damage gets state 2.
fn rebuild(
    dir: &Path,
    catalog: &mut CatalogWriter,
    slot: u64,
    found: &Descriptor,
    recovery: &mut Recovery,
) -> Result<(), DirError> {
    let id = found.recording_id();
    let start = found.start_position();
    let mut entries = repair_index(dir, found, recovery)?;
    let segments = list_segments(dir, id)?;
    let mut damage = false;
    let mut stop = None;
    if segments.is_empty() {
        stop = Some(start);
    } else {
        let start_located = locate(found, start)?;
        match segments
            .iter()
            .find(|segment| segment.base == start_located.segment_base_position)
        {
            None => damage = true,
            Some(segment) => match frame_at(segment, found, &start_located)? {
                Frame::Zero => stop = Some(start),
                Frame::Fails => damage = true,
                Frame::Checks(_) => {}
            },
        }
        if !damage {
            let segment_length = i64::from(found.segment_file_length());
            let last_base = segments
                .last()
                .map_or(start_located.segment_base_position, |s| s.base);
            let mut base = start_located.segment_base_position;
            while base < last_base {
                base += segment_length;
                if !segments.iter().any(|segment| segment.base == base) {
                    damage = true;
                    break;
                }
            }
        }
    }
    if damage {
        catalog.mark_invalid(slot)?;
        recovery.invalidated += 1;
        return Ok(());
    }
    let stop = match stop {
        Some(stop) => stop,
        None => {
            let mut from = start;
            for entry in entries.iter().rev() {
                if entry.position >= start && frame_checks(&segments, found, entry.position)? {
                    from = entry.position;
                    break;
                }
            }
            scan(&segments, found, from)?
        }
    };
    let kept = entries
        .iter()
        .take_while(|entry| entry.position <= stop)
        .count();
    if kept < entries.len() {
        // Index entries past the stop are dropped: one more rewrite of the index.
        recovery.index_rewritten += 1;
        entries.truncate(kept);
        let index_path = dir.join(index_file_name(id));
        let index = OpenOptions::new()
            .write(true)
            .open(&index_path)
            .map_err(io("open", &index_path))?;
        index
            .set_len(HEADER_LENGTH as u64 + (kept as u64) * ENTRY_LENGTH as u64)
            .map_err(io("truncate", &index_path))?;
        index.sync_all().map_err(io("sync", &index_path))?;
    }
    let stop_timestamp = entries
        .last()
        .map_or(found.start_timestamp(), |entry| entry.timestamp);
    catalog.rewrite(slot, &found.stopped(stop, stop_timestamp)?)?;
    recovery.recovered += 1;
    recovery.files_removed += remove_files(dir, id, stop)?;
    Ok(())
}

/// What the frame at a located position says.
enum Frame {
    /// The frame length there is 0, or the position is past what the segment holds.
    Zero,
    /// A frame is there and fails the `frame` check.
    Fails,
    /// A frame is there and checks: its aligned length.
    Checks(i64),
}

/// The `frame` check at `located`, in `segment` (`spec/durable-log.md`, "What a reader
/// rejects"): the header carries the position's term id and term offset and the
/// descriptor's session id and stream id, and the frame lies within the segment file.
fn frame_at(segment: &Segment, found: &Descriptor, located: &Located) -> Result<Frame, DirError> {
    let offset = located.offset_in_segment as u64;
    if offset + FRAME_HEADER_LENGTH as u64 > segment.length {
        return Ok(Frame::Zero);
    }
    let file = File::open(&segment.path).map_err(io("open", &segment.path))?;
    let mut bytes = [0u8; FRAME_HEADER_LENGTH];
    file.read_exact_at(&mut bytes, offset)
        .map_err(io("read a frame of", &segment.path))?;
    let raw_length = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if raw_length == 0 {
        return Ok(Frame::Zero);
    }
    // The bytes left in the term from the frame's start, as a log buffer reader passes
    // them (`spec/log-buffer.md`, "Reading"): the length check runs against those.
    let left_in_term = (found.term_length() - located.term_offset) as usize;
    let Ok(header) = DataFrameHeader::parse_within(&bytes, left_in_term) else {
        return Ok(Frame::Fails);
    };
    let aligned = header.aligned_length() as i64;
    let within_file = offset + aligned as u64 <= segment.length;
    if header.term_id() != located.term_id
        || header.term_offset() != located.term_offset
        || header.session_id() != found.session_id()
        || header.stream_id() != found.stream_id()
        || !within_file
    {
        return Ok(Frame::Fails);
    }
    Ok(Frame::Checks(aligned))
}

/// Whether the frame at `position` checks, with its segment present.
fn frame_checks(segments: &[Segment], found: &Descriptor, position: i64) -> Result<bool, DirError> {
    let located = match locate(found, position) {
        Ok(located) => located,
        Err(_) => return Ok(false),
    };
    match segments
        .iter()
        .find(|segment| segment.base == located.segment_base_position)
    {
        Some(segment) => Ok(matches!(
            frame_at(segment, found, &located)?,
            Frame::Checks(_)
        )),
        None => Ok(false),
    }
}

/// The scan: frame by frame from `from`, to the end of the last segment file at most,
/// stopping at the first frame that fails the check or whose length is 0; returns the
/// end of the last frame that checks.
fn scan(segments: &[Segment], found: &Descriptor, from: i64) -> Result<i64, DirError> {
    let mut position = from;
    loop {
        let located = locate(found, position)?;
        let Some(segment) = segments
            .iter()
            .find(|segment| segment.base == located.segment_base_position)
        else {
            return Ok(position);
        };
        match frame_at(segment, found, &located)? {
            Frame::Checks(aligned) => position += aligned,
            Frame::Zero | Frame::Fails => return Ok(position),
        }
    }
}

/// The index repair: a missing or torn index is written again with the start entry, an
/// index with no whole entry gets it, and the file is truncated to whole entries in
/// order. Returns the entries left, at least one.
fn repair_index(
    dir: &Path,
    found: &Descriptor,
    recovery: &mut Recovery,
) -> Result<Vec<IndexEntry>, DirError> {
    let id = found.recording_id();
    let path = dir.join(index_file_name(id));
    let first = IndexEntry {
        timestamp: found.start_timestamp(),
        position: found.start_position(),
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(io("open", &path))?;
    let length = file.metadata().map_err(io("stat", &path))?.len();
    let header_ok = if length >= HEADER_LENGTH as u64 {
        let mut bytes = [0u8; HEADER_LENGTH];
        file.read_exact_at(&mut bytes, 0)
            .map_err(io("read the header of", &path))?;
        matches!(IndexHeader::parse(&bytes, length), Ok(header) if header.recording_id() == id)
    } else {
        false
    };
    if !header_ok {
        write_index(&file, &path, id, &first)?;
        recovery.index_rewritten += 1;
        return Ok(vec![first]);
    }
    let whole = (length - HEADER_LENGTH as u64) / ENTRY_LENGTH as u64;
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(whole as usize);
    let mut bytes = [0u8; ENTRY_LENGTH];
    for j in 0..whole {
        file.read_exact_at(&mut bytes, HEADER_LENGTH as u64 + j * ENTRY_LENGTH as u64)
            .map_err(io("read an entry of", &path))?;
        match IndexEntry::parse(&bytes, entries.last()) {
            Ok(entry) => entries.push(entry),
            Err(_) => break,
        }
    }
    let mut changed = false;
    if entries.is_empty() {
        file.write_all_at(&first.encode(), HEADER_LENGTH as u64)
            .map_err(io("write the first entry of", &path))?;
        entries.push(first);
        changed = true;
    }
    let kept = HEADER_LENGTH as u64 + entries.len() as u64 * ENTRY_LENGTH as u64;
    if kept != length {
        file.set_len(kept).map_err(io("truncate", &path))?;
        changed = true;
    }
    if changed {
        file.sync_all().map_err(io("sync", &path))?;
        recovery.index_rewritten += 1;
    }
    Ok(entries)
}

/// Writes an index anew in the page's order: the fields, a sync, the magic, the first
/// entry, a sync.
fn write_index(file: &File, path: &Path, id: i64, first: &IndexEntry) -> Result<(), DirError> {
    let header = IndexHeader::new(id)?.encode();
    file.set_len(0).map_err(io("truncate", path))?;
    file.write_all_at(&header[8..], 8)
        .map_err(io("write the header of", path))?;
    file.sync_all().map_err(io("sync", path))?;
    file.write_all_at(&header[..8], 0)
        .map_err(io("write the magic of", path))?;
    file.write_all_at(&first.encode(), HEADER_LENGTH as u64)
        .map_err(io("write the first entry of", path))?;
    file.sync_all().map_err(io("sync", path))?;
    Ok(())
}

/// The recording's segment files, by base position.
fn list_segments(dir: &Path, id: i64) -> Result<Vec<Segment>, DirError> {
    let prefix = format!("{id}-");
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir).map_err(io("list", dir))? {
        let entry = entry.map_err(io("list", dir))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(base) = rest.strip_suffix(SEGMENT_SUFFIX) else {
            continue;
        };
        if base.is_empty() || !base.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(base) = base.parse::<i64>() else {
            continue;
        };
        let path = entry.path();
        let length = entry.metadata().map_err(io("stat", &path))?.len();
        segments.push(Segment { base, path, length });
    }
    segments.sort_by_key(|segment| segment.base);
    Ok(segments)
}

/// Removes the recording's segment files whose base position is at or past `from`,
/// which lie wholly past a stop there, and with `i64::MIN` every file of the recording,
/// its index included. Returns how many files went.
fn remove_files(dir: &Path, id: i64, from: i64) -> Result<u64, DirError> {
    let mut removed = 0;
    for segment in list_segments(dir, id)? {
        if segment.base >= from {
            fs::remove_file(&segment.path).map_err(io("remove", &segment.path))?;
            removed += 1;
        }
    }
    if from == i64::MIN {
        let index = dir.join(format!("{id}{INDEX_SUFFIX}"));
        if index.exists() {
            fs::remove_file(&index).map_err(io("remove", &index))?;
            removed += 1;
        }
    }
    Ok(removed)
}

const _: () = assert!(FRAME_ALIGNMENT == 32);
