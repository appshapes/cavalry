//! Reading a recording's time index: an instant to a position, by the rule of
//! `spec/durable-log.md`, "Time index" (ADR 0024, ADR 0030). The index is a file any
//! reader may read without a lock: the recorder appends whole entries and only the last
//! can be part-written, so a reader checks the last entry and ignores one that is torn
//! or breaks the order. [`position_at`] is what `cavalry replay --from <instant>`
//! resolves, [`position_after`] what `--to <instant>` resolves; both search the entries
//! by their timestamps, which never decrease, with a handful of reads of one entry each.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::layout::{
    ENTRY_LENGTH, HEADER_LENGTH, IndexEntry, IndexHeader, LayoutError, index_file_name,
};

const DOC: &str = "docs/reference/durable-log.md#time-index";

/// Why an instant could not be resolved.
#[derive(Debug)]
pub enum IndexError {
    /// The operating system refused a step on the index file.
    Io {
        /// The step.
        what: &'static str,
        /// The path.
        path: PathBuf,
        /// The error.
        source: io::Error,
    },
    /// The header, or an entry, failed a check of the page.
    Layout {
        /// The path.
        path: PathBuf,
        /// The check that failed.
        source: LayoutError,
    },
    /// The index names another recording than the file name says.
    Recording {
        /// The path.
        path: PathBuf,
        /// The recording id in the header.
        found: i64,
        /// The recording id asked for.
        expected: i64,
    },
    /// The index holds no whole entry: the recorder has not written its first entry, or
    /// the file was torn.
    Empty {
        /// The path.
        path: PathBuf,
    },
}

impl IndexError {
    /// The reason's name: `io`, the layout check's name, `recording-id` or `empty`.
    pub const fn reason(&self) -> &'static str {
        match self {
            IndexError::Io { .. } => "io",
            IndexError::Layout { source, .. } => source.reason(),
            IndexError::Recording { .. } => "recording-id",
            IndexError::Empty { .. } => "empty",
        }
    }
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Io { what, path, source } => write!(
                f,
                "time index refused (io): cannot {what} {}: {source}; the recording's index is \
                 missing or unreadable, so an instant cannot be resolved: pass a position, or \
                 check the log directory with `cavalry doctor`; see {DOC}",
                path.display()
            ),
            IndexError::Layout { path, source } => write!(
                f,
                "time index rejected ({}): {} failed a check: {source}; the file is damaged, \
                 and `cavalry doctor` names the recording; pass a position instead; see {DOC}",
                source.reason(),
                path.display()
            ),
            IndexError::Recording {
                path,
                found,
                expected,
            } => write!(
                f,
                "time index rejected (recording-id): {} names recording {found}, not {expected}; \
                 the log directory's files were moved or renamed; see {DOC}",
                path.display()
            ),
            IndexError::Empty { path } => write!(
                f,
                "time index rejected (empty): {} holds no whole entry; the recording has not \
                 written its first entry yet, or the file was torn; wait a millisecond, or pass \
                 a position; see {DOC}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::Io { source, .. } => Some(source),
            IndexError::Layout { source, .. } => Some(source),
            IndexError::Recording { .. } | IndexError::Empty { .. } => None,
        }
    }
}

/// A recording's index, open for reading: the file and how many whole entries in
/// order it holds, the last one checked against the one before it.
struct Index {
    file: File,
    path: PathBuf,
    count: u64,
}

impl Index {
    fn open(dir: &Path, recording_id: i64) -> Result<Self, IndexError> {
        let path = dir.join(index_file_name(recording_id));
        let io = |what: &'static str, source: io::Error| IndexError::Io {
            what,
            path: path.clone(),
            source,
        };
        let file = File::open(&path).map_err(|source| io("open", source))?;
        let length = file.metadata().map_err(|source| io("stat", source))?.len();
        let mut header = [0u8; HEADER_LENGTH];
        if length >= HEADER_LENGTH as u64 {
            file.read_exact_at(&mut header, 0)
                .map_err(|source| io("read the header of", source))?;
        }
        let parsed = IndexHeader::parse(&header, length).map_err(|source| IndexError::Layout {
            path: path.clone(),
            source,
        })?;
        if parsed.recording_id() != recording_id {
            return Err(IndexError::Recording {
                path,
                found: parsed.recording_id(),
                expected: recording_id,
            });
        }
        let mut index = Index {
            file,
            path,
            count: IndexHeader::entries(length),
        };
        // Only the last entry can be part-written or break the order: it is checked
        // against the one before it, and dropped when it fails.
        if index.count >= 2 {
            let previous = index.read(index.count - 2)?;
            if index.entry(index.count - 1, Some(&previous)).is_err() {
                index.count -= 1;
            }
        } else if index.count == 1 && index.entry(0, None).is_err() {
            index.count = 0;
        }
        if index.count == 0 {
            return Err(IndexError::Empty { path: index.path });
        }
        Ok(index)
    }

    /// Entry `j`, checked against `previous` when given.
    fn entry(&self, j: u64, previous: Option<&IndexEntry>) -> Result<IndexEntry, IndexError> {
        let mut bytes = [0u8; ENTRY_LENGTH];
        self.file
            .read_exact_at(&mut bytes, HEADER_LENGTH as u64 + j * ENTRY_LENGTH as u64)
            .map_err(|source| IndexError::Io {
                what: "read an entry of",
                path: self.path.clone(),
                source,
            })?;
        IndexEntry::parse(&bytes, previous).map_err(|source| IndexError::Layout {
            path: self.path.clone(),
            source,
        })
    }

    fn read(&self, j: u64) -> Result<IndexEntry, IndexError> {
        self.entry(j, None)
    }

    /// How many entries have a timestamp at or before `instant`: the entries are in
    /// timestamp order, so a binary search over them.
    fn at_or_before(&self, instant: i64) -> Result<u64, IndexError> {
        let (mut low, mut high) = (0u64, self.count);
        while low < high {
            let mid = low + (high - low) / 2;
            if self.read(mid)?.timestamp <= instant {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        Ok(low)
    }
}

/// The position a replay from `instant` starts at: the position of the last entry
/// whose timestamp is at or before the instant, or the start position, the first
/// entry's, when the instant is before every entry (`spec/durable-log.md`, "Time
/// index"). `instant` is nanoseconds since the Unix epoch, the recorder's clock. The
/// granularity is the recorder's cadence, one millisecond.
pub fn position_at(dir: &Path, recording_id: i64, instant: i64) -> Result<i64, IndexError> {
    let index = Index::open(dir, recording_id)?;
    let before = index.at_or_before(instant)?;
    let j = before.saturating_sub(1);
    Ok(index.read(j)?.position)
}

/// Truncates the index to the entries at or before `position`, a truncate's cut of the
/// index (`spec/durable-log.md`, "The driver"; ADR 0031), and returns the timestamp of
/// the last entry left, or `None` when no entry is at or before the position, in which
/// case the file keeps its first entry alone, since one writer never leaves an index
/// with none. The file is synced after the cut.
pub fn truncate_at(
    dir: &Path,
    recording_id: i64,
    position: i64,
) -> Result<Option<i64>, IndexError> {
    let index = Index::open(dir, recording_id)?;
    // Positions never decrease from one entry to the next: the first entry past the
    // position is found by a search, as the recovery pass finds it.
    let (mut low, mut high) = (0u64, index.count);
    while low < high {
        let mid = low + (high - low) / 2;
        if index.read(mid)?.position <= position {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let keep = low.max(1);
    let io_error = |what: &'static str| {
        let path = index.path.clone();
        move |source| IndexError::Io { what, path, source }
    };
    if keep < index.count {
        // The reader's handle is read-only; the cut needs one that writes.
        let writable = std::fs::OpenOptions::new()
            .write(true)
            .open(&index.path)
            .map_err(io_error("open"))?;
        writable
            .set_len(HEADER_LENGTH as u64 + keep * ENTRY_LENGTH as u64)
            .map_err(io_error("truncate"))?;
        writable.sync_all().map_err(io_error("sync"))?;
    }
    if low == 0 {
        return Ok(None);
    }
    Ok(Some(index.read(keep - 1)?.timestamp))
}

/// The position a replay to `instant` ends at: the position of the first entry whose
/// timestamp is after the instant, or the last entry's position when there is none.
pub fn position_after(dir: &Path, recording_id: i64, instant: i64) -> Result<i64, IndexError> {
    let index = Index::open(dir, recording_id)?;
    let before = index.at_or_before(instant)?;
    let j = before.min(index.count - 1);
    Ok(index.read(j)?.position)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(test: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("cavalry-log-index-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("a temporary directory");
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Writes recording 3's index with `entries` as (timestamp, position) pairs and
    /// `extra` bytes after them.
    fn write_index(dir: &Path, entries: &[(i64, i64)], extra: &[u8]) {
        let mut bytes = IndexHeader::new(3).unwrap().encode().to_vec();
        for &(timestamp, position) in entries {
            bytes.extend_from_slice(
                &IndexEntry {
                    timestamp,
                    position,
                }
                .encode(),
            );
        }
        bytes.extend_from_slice(extra);
        fs::write(dir.join(index_file_name(3)), bytes).unwrap();
    }

    const ENTRIES: [(i64, i64); 4] = [(100, 0), (200, 64), (200, 128), (300, 192)];

    #[test]
    fn an_instant_resolves_to_the_last_entry_at_or_before_it_and_to_the_first_after_it() {
        let dir = Dir::new("resolve");
        write_index(&dir.0, &ENTRIES, &[]);
        for (instant, at, after) in [
            (50, 0, 0),
            (100, 0, 64),
            (150, 0, 64),
            (200, 128, 192),
            (250, 128, 192),
            (300, 192, 192),
            (i64::MAX, 192, 192),
        ] {
            assert_eq!(position_at(&dir.0, 3, instant).unwrap(), at, "at {instant}");
            assert_eq!(
                position_after(&dir.0, 3, instant).unwrap(),
                after,
                "after {instant}"
            );
        }
    }

    #[test]
    fn a_torn_or_out_of_order_last_entry_is_ignored() {
        let dir = Dir::new("torn");
        write_index(&dir.0, &ENTRIES, &[7u8; 9]);
        assert_eq!(
            position_at(&dir.0, 3, 400).unwrap(),
            192,
            "bytes past the last whole entry"
        );
        let mut broken = ENTRIES.to_vec();
        broken.push((250, 256));
        write_index(&dir.0, &broken, &[]);
        assert_eq!(
            position_at(&dir.0, 3, 400).unwrap(),
            192,
            "a last entry that breaks the order"
        );
        assert_eq!(position_after(&dir.0, 3, 400).unwrap(), 192);
    }

    #[test]
    fn a_missing_empty_or_wrong_index_is_refused_by_name() {
        let dir = Dir::new("refused");
        let error = position_at(&dir.0, 3, 0).unwrap_err();
        assert_eq!(error.reason(), "io", "{error}");
        write_index(&dir.0, &[], &[]);
        let error = position_at(&dir.0, 3, 0).unwrap_err();
        assert_eq!(error.reason(), "empty", "{error}");
        write_index(&dir.0, &[(100, 0)], &[]);
        assert_eq!(position_at(&dir.0, 3, 0).unwrap(), 0);
        assert_eq!(position_after(&dir.0, 3, 0).unwrap(), 0);
        fs::rename(
            dir.0.join(index_file_name(3)),
            dir.0.join(index_file_name(4)),
        )
        .unwrap();
        let error = position_at(&dir.0, 4, 0).unwrap_err();
        assert_eq!(error.reason(), "recording-id", "{error}");
        fs::write(dir.0.join(index_file_name(5)), b"not an index").unwrap();
        let error = position_at(&dir.0, 5, 0).unwrap_err();
        assert_eq!(error.reason(), "too-short", "{error}");
        for error in [
            position_at(&dir.0, 3, 0).unwrap_err(),
            position_at(&dir.0, 4, 0).unwrap_err(),
            position_at(&dir.0, 5, 0).unwrap_err(),
        ] {
            assert!(error.to_string().contains(DOC), "{error}");
        }
    }
}
