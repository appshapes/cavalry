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

/// How many time index entries the repair reads at a time: 16 KiB on the stack,
/// whatever the index's length, so that the pass holds a fixed amount of memory for a
/// file of any size (invariant 3). A recording live for a day leaves about 86 million
/// entries, 1.4 GB, which is read once, forward, and never held.
const INDEX_CHUNK: usize = 1024;

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
/// transition the driver counts (`counters.toml`; ADR 0023, card 93), and each happens
/// once: a later open of a repaired directory reports nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recovery {
    /// The catalog held bytes past its last whole slot, left by a crash while it grew,
    /// and was truncated to whole slots.
    pub truncated: bool,
    /// The catalog's last slot was empty, a creation torn by a crash, and was dropped
    /// from the file, so the next recording takes its id.
    pub dropped_empty_slot: bool,
    /// Recordings live at a crash, or whose stop was torn, whose stop fields were rebuilt
    /// from their files.
    pub recovered: u64,
    /// Slots set to state 2: an empty slot before the last, a creation checksum that
    /// fails, a state the page does not name, or files that cannot repair the recording.
    pub invalidated: u64,
    /// Time indexes written again with their first entry, given a first entry, or
    /// truncated to whole entries in order or to the entries at or before the stop.
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

    /// Runs the recovery pass for one recording's slot, as the driver does after a
    /// retention command's file work failed, so that the slot and the files agree before
    /// the refusal is answered (ADR 0031): the same rules as at start-up, counted the
    /// same way. A recording id past the last slot is `unknown`.
    pub fn repair(&mut self, recording_id: i64) -> Result<Recovery, DirError> {
        let mut recovery = Recovery::default();
        let slot = match u64::try_from(recording_id) {
            Ok(slot) if slot < self.catalog.slots() => slot,
            _ => return Err(CatalogError::Unknown { recording_id }.into()),
        };
        let dir = self.dir.clone();
        recover_slot(&dir, &mut self.catalog, slot, &mut recovery)?;
        Ok(recovery)
    }

    /// The catalog side of a slot's repair, the write [`repair_files`] returned, on the
    /// thread that owns the catalog: one slot, synced, counted as the transition it is
    /// (ADR 0031). A recording id past the last slot is `unknown`.
    pub fn apply_repair(
        &mut self,
        recording_id: i64,
        write: &SlotWrite,
    ) -> Result<Recovery, DirError> {
        let mut recovery = Recovery::default();
        let slot = match u64::try_from(recording_id) {
            Ok(slot) if slot < self.catalog.slots() => slot,
            _ => return Err(CatalogError::Unknown { recording_id }.into()),
        };
        apply_slot_write(&mut self.catalog, slot, write, &mut recovery)?;
        Ok(recovery)
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
/// truncated the file to whole slots and dropped an empty last slot; `before` is the
/// file's length before that, which says what it did.
fn recover(dir: &Path, catalog: &mut CatalogWriter, before: u64) -> Result<Recovery, DirError> {
    let mut recovery = Recovery::default();
    let header = HEADER_LENGTH as u64;
    let descriptor = DESCRIPTOR_LENGTH as u64;
    recovery.truncated = before > header && !(before - header).is_multiple_of(descriptor);
    recovery.dropped_empty_slot =
        before >= header && (before - header) / descriptor > catalog.slots();
    for slot in 0..catalog.slots() {
        recover_slot(dir, catalog, slot, &mut recovery)?;
    }
    Ok(recovery)
}

/// What the recovery pass found a slot needs written once its file side has run: the
/// one catalog write a repair makes after a retention command's file work failed,
/// on the thread that owns the catalog, while the file side ran on the replayer
/// (ADR 0031). At start-up the same plan is applied at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotWrite {
    /// The slot and the files agree: nothing to write.
    Nothing,
    /// Damage the files cannot repair: state 2.
    Invalidate,
    /// What recovery rebuilt from the files, the retained group first, then the stop,
    /// either or both; boxed, since a descriptor is large and the write is one per
    /// repair, off every message path.
    Rebuilt(Box<RebuiltSlot>),
}

/// The descriptors a rebuilt slot is written from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuiltSlot {
    /// The descriptor with the retained group rebuilt, when the group was torn.
    pub retained: Option<Descriptor>,
    /// The descriptor with the stop rebuilt, when the recording read live or its stop
    /// was torn.
    pub stopped: Option<Descriptor>,
}

/// The recovery pass for one slot, the page's rules in order (`spec/durable-log.md`,
/// "The driver"): at start-up for every slot, the file side and the catalog write at
/// once; a retention command's failed file work runs the two apart, the file side on
/// the replayer through [`repair_files`] and the write on the recorder through
/// [`LogDir::apply_repair`] (ADR 0031).
fn recover_slot(
    dir: &Path,
    catalog: &mut CatalogWriter,
    slot: u64,
    recovery: &mut Recovery,
) -> Result<(), DirError> {
    let bytes = catalog.slot_bytes(slot)?;
    let write = plan_slot(dir, slot, &bytes, recovery)?;
    apply_slot_write(catalog, slot, &write, recovery)
}

/// The file side of one slot's recovery, from the slot's bytes as the catalog holds
/// them: what the rules find, the index written again where it needs it, and the files
/// a rebuilt stop or a deleted slot leaves behind removed; counted in `recovery`. The
/// catalog is not touched: the write the rules end in is returned for
/// [`apply_slot_write`]. The file side runs first, so a crash between the two leaves
/// files the next pass reads to the same end.
fn plan_slot(
    dir: &Path,
    slot: u64,
    bytes: &[u8; DESCRIPTOR_LENGTH],
    recovery: &mut Recovery,
) -> Result<SlotWrite, DirError> {
    let state = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    match State::from_i32(state) {
        Some(State::Valid | State::Deleted) => {}
        Some(State::Invalid) => return Ok(SlotWrite::Nothing),
        // An empty slot before the last cannot come from one writer, and a state the
        // page does not name is damage too.
        Some(State::Empty) | None => return Ok(SlotWrite::Invalidate),
    }
    let id = i64::try_from(slot).unwrap_or(i64::MAX);
    let (found, torn) = match Descriptor::parse_for_recovery(bytes, id) {
        Ok(Some(parsed)) => parsed,
        // A creation checksum or a creation field that fails: damage the files
        // cannot repair, and recovery never writes a creation field.
        Ok(None) | Err(_) => return Ok(SlotWrite::Invalidate),
    };
    if found.state() == State::Deleted {
        // A deleted recording whose files are still present loses them again.
        recovery.files_removed += remove_files(dir, id, i64::MIN)?;
        return Ok(SlotWrite::Nothing);
    }
    // A retained group torn by a crash inside a migration or a purge is rebuilt from
    // the files: the larger of the start position and the base of the lowest segment
    // file present, or the start position when none is (ADR 0031).
    let (found, retained) = if torn.retained {
        let lowest = list_segments(dir, id)?.first().map(|segment| segment.base);
        let retained = lowest.map_or(found.start_position(), |base| {
            base.max(found.start_position())
        });
        let rebuilt = found.retained(retained)?;
        (rebuilt.clone(), Some(rebuilt))
    } else {
        (found, None)
    };
    // Segment files wholly before the retained position are a purge's leftovers,
    // removed and never read. Until a purge moved the position past the start no file
    // can lie before it, so a healthy slot costs no listing of the directory.
    if found.retained_position() > found.start_position() {
        recovery.files_removed += remove_leading_files(dir, id, found.retained_position())?;
    }
    if !torn.stop && !found.is_live() {
        return Ok(match retained {
            Some(_) => SlotWrite::Rebuilt(Box::new(RebuiltSlot {
                retained,
                stopped: None,
            })),
            None => SlotWrite::Nothing,
        });
    }
    match rebuild(dir, &found, recovery)? {
        Rebuilt::Stopped(stopped) => Ok(SlotWrite::Rebuilt(Box::new(RebuiltSlot {
            retained,
            stopped: Some(stopped),
        }))),
        Rebuilt::Damage => Ok(SlotWrite::Invalidate),
    }
}

/// The catalog side of one slot's recovery: the write [`plan_slot`] ended in, counted
/// in `recovery` as the transition it is.
fn apply_slot_write(
    catalog: &mut CatalogWriter,
    slot: u64,
    write: &SlotWrite,
    recovery: &mut Recovery,
) -> Result<(), DirError> {
    match write {
        SlotWrite::Nothing => Ok(()),
        SlotWrite::Invalidate => {
            catalog.mark_invalid(slot)?;
            recovery.invalidated += 1;
            Ok(())
        }
        SlotWrite::Rebuilt(rebuilt) => {
            if let Some(retained) = &rebuilt.retained {
                catalog.rewrite_retained(slot, retained)?;
                recovery.recovered += 1;
            }
            if let Some(stopped) = &rebuilt.stopped {
                catalog.rewrite(slot, stopped)?;
                recovery.recovered += 1;
            }
            Ok(())
        }
    }
}

/// The file side of a slot's repair after a retention command's file work failed, run
/// on the replayer, which has the files open (ADR 0031): the recovery pass's rules on
/// `bytes`, the slot as a reader holds it, with the index and the files put right and
/// what they did counted; the returned write is the one the recorder makes through
/// [`LogDir::apply_repair`]. A recording id below 0 is `unknown`.
pub fn repair_files(
    dir: &Path,
    recording_id: i64,
    bytes: &[u8; DESCRIPTOR_LENGTH],
) -> Result<(SlotWrite, Recovery), DirError> {
    let slot = u64::try_from(recording_id).map_err(|_| CatalogError::Unknown { recording_id })?;
    let mut recovery = Recovery::default();
    let write = plan_slot(dir, slot, bytes, &mut recovery)?;
    Ok((write, recovery))
}

/// What rebuilding a stop from the files found.
enum Rebuilt {
    /// The descriptor with the stop the files give.
    Stopped(Descriptor),
    /// The files cannot repair the recording: state 2.
    Damage,
}

/// Rebuilds the stop fields of a recording live at a crash or whose stop was torn, from
/// its files, in the page's order: the index first, then an empty recording, then the
/// scan's start, then the scan; only damage gets state 2. The files past the stop go
/// here; the stop itself is the caller's write.
fn rebuild(dir: &Path, found: &Descriptor, recovery: &mut Recovery) -> Result<Rebuilt, DirError> {
    let id = found.recording_id();
    // The recording's bytes begin at the retained position: the start position until a
    // purge moved it (ADR 0031); every rule below counts from there.
    let start = found.retained_position();
    let mut index = repair_index(dir, found, recovery)?;
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
        return Ok(Rebuilt::Damage);
    }
    let stop = match stop {
        Some(stop) => stop,
        None => {
            // The scan's start: the last index entry at whose position a frame checks,
            // walking back entry by entry from the file, or the start position.
            let mut from = start;
            let mut j = index.count;
            while j > 0 {
                j -= 1;
                let entry = index.read(j)?;
                if entry.position >= start && frame_checks(&segments, found, entry.position)? {
                    from = entry.position;
                    break;
                }
            }
            scan(&segments, found, from)?
        }
    };
    // The cut: entries past the stop are dropped. Positions never decrease from one
    // entry to the next, so the first entry past the stop is found by a search over the
    // file, in a number of reads that grows with the logarithm of the count.
    let (mut low, mut high) = (0u64, index.count);
    while low < high {
        let mid = low + (high - low) / 2;
        if index.read(mid)?.position <= stop {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let mut changed = false;
    if low < index.count {
        index.truncate(low)?;
        changed = true;
    }
    if index.count == 0 {
        // The first entry lay past the stop, which one writer never leaves behind: the
        // start entry takes its place.
        index.write_first(found)?;
        changed = true;
    }
    if changed {
        recovery.index_rewritten += 1;
    }
    // An empty recording's stop timestamp is the start timestamp, whatever its first
    // entry carries (`spec/durable-log.md`, "The driver"; ADR 0031); otherwise the last
    // entry left gives it.
    let stop_timestamp = if stop <= start {
        found.start_timestamp()
    } else {
        index.read(index.count - 1)?.timestamp
    };
    let stopped = found.stopped(stop, stop_timestamp)?;
    recovery.files_removed += remove_files(dir, id, stop)?;
    Ok(Rebuilt::Stopped(stopped))
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

/// A recording's time index, open for the repair: the file and how many whole entries
/// in order it holds. Entries are read from the file one at a time; none are held.
struct Index {
    file: File,
    path: PathBuf,
    count: u64,
}

impl Index {
    fn read(&self, j: u64) -> Result<IndexEntry, DirError> {
        let mut bytes = [0u8; ENTRY_LENGTH];
        self.file
            .read_exact_at(&mut bytes, HEADER_LENGTH as u64 + j * ENTRY_LENGTH as u64)
            .map_err(io("read an entry of", &self.path))?;
        Ok(IndexEntry::parse(&bytes, None)?)
    }

    fn truncate(&mut self, count: u64) -> Result<(), DirError> {
        self.file
            .set_len(HEADER_LENGTH as u64 + count * ENTRY_LENGTH as u64)
            .map_err(io("truncate", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        self.count = count;
        Ok(())
    }

    /// The start entry as the only entry: the start timestamp at the start position.
    fn write_first(&mut self, found: &Descriptor) -> Result<(), DirError> {
        let first = IndexEntry {
            timestamp: found.start_timestamp(),
            position: found.start_position(),
        };
        self.file
            .write_all_at(&first.encode(), HEADER_LENGTH as u64)
            .map_err(io("write the first entry of", &self.path))?;
        self.file.sync_all().map_err(io("sync", &self.path))?;
        self.count = 1;
        Ok(())
    }
}

/// The index repair: a missing or torn index is written again with the start entry, an
/// index with no whole entry gets it, and the file is truncated to whole entries in
/// order, checked forward in fixed chunks with only the entry before kept.
fn repair_index(
    dir: &Path,
    found: &Descriptor,
    recovery: &mut Recovery,
) -> Result<Index, DirError> {
    let id = found.recording_id();
    let path = dir.join(index_file_name(id));
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
    let mut index = Index {
        file,
        path,
        count: 0,
    };
    if !header_ok {
        write_index(&index.file, &index.path, id)?;
        index.write_first(found)?;
        recovery.index_rewritten += 1;
        return Ok(index);
    }
    let whole = (length - HEADER_LENGTH as u64) / ENTRY_LENGTH as u64;
    let mut buffer = [0u8; INDEX_CHUNK * ENTRY_LENGTH];
    let mut previous: Option<IndexEntry> = None;
    let mut count = 0u64;
    'entries: while count < whole {
        let n = (whole - count).min(INDEX_CHUNK as u64) as usize;
        index
            .file
            .read_exact_at(
                &mut buffer[..n * ENTRY_LENGTH],
                HEADER_LENGTH as u64 + count * ENTRY_LENGTH as u64,
            )
            .map_err(io("read the entries of", &index.path))?;
        for chunk in buffer[..n * ENTRY_LENGTH].as_chunks::<ENTRY_LENGTH>().0 {
            match IndexEntry::parse(chunk, previous.as_ref()) {
                Ok(entry) => {
                    previous = Some(entry);
                    count += 1;
                }
                Err(_) => break 'entries,
            }
        }
    }
    index.count = count;
    let mut changed = false;
    if count == 0 {
        index.write_first(found)?;
        changed = true;
    }
    let kept = HEADER_LENGTH as u64 + index.count * ENTRY_LENGTH as u64;
    if kept != length {
        index
            .file
            .set_len(kept)
            .map_err(io("truncate", &index.path))?;
        index.file.sync_all().map_err(io("sync", &index.path))?;
        changed = true;
    }
    if changed {
        recovery.index_rewritten += 1;
    }
    Ok(index)
}

/// Writes an index header anew in the page's order: the fields, a sync, then the magic;
/// the first entry follows.
fn write_index(file: &File, path: &Path, id: i64) -> Result<(), DirError> {
    let header = IndexHeader::new(id)?.encode();
    file.set_len(0).map_err(io("truncate", path))?;
    file.write_all_at(&header[8..], 8)
        .map_err(io("write the header of", path))?;
    file.sync_all().map_err(io("sync", path))?;
    file.write_all_at(&header[..8], 0)
        .map_err(io("write the magic of", path))?;
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

/// Removes the recording's segment files whose bytes lie wholly before `retained`, a
/// purge's leftovers after a crash; returns how many went (ADR 0031).
fn remove_leading_files(dir: &Path, id: i64, retained: i64) -> Result<u64, DirError> {
    let mut removed = 0;
    for segment in list_segments(dir, id)? {
        let length = i64::try_from(segment.length).unwrap_or(i64::MAX);
        if segment.base.saturating_add(length) <= retained && segment.base < retained {
            fs::remove_file(&segment.path).map_err(io("remove", &segment.path))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// The recording's segment base positions, from its files in the directory, in no
/// order; what a retention command's file work walks (ADR 0031).
pub fn segment_bases(dir: &Path, recording_id: i64) -> Result<Vec<i64>, DirError> {
    Ok(list_segments(dir, recording_id)?
        .into_iter()
        .map(|segment| segment.base)
        .collect())
}

/// Whether a frame begins at `position` of the recording `found` describes: the frame
/// header there passes the `frame` check, which a truncate requires of its position
/// below the stop (ADR 0031). A position past the files, or at which the length reads
/// 0, is `Ok(false)`; a file the operating system refuses is the error.
pub fn frame_begins_at(dir: &Path, found: &Descriptor, position: i64) -> Result<bool, DirError> {
    let segments = list_segments(dir, found.recording_id())?;
    frame_checks(&segments, found, position)
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
