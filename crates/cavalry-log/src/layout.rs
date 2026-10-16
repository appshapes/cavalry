//! The durable log's on-disk layouts, version 0: the catalog header and its descriptors,
//! the time index header and its entries, and the arithmetic from a position to its
//! segment file (`spec/durable-log.md`; ADR 0024). Pure functions over byte slices, no
//! file and no `unsafe`, so that the conformance vectors and the no-panic test run on
//! every commit and under Miri. What a reader rejects is reported by name, in the
//! order the page gives, as [`LayoutError::reason`].

use std::fmt;

/// The catalog's file name inside the log directory.
pub const CATALOG_FILE_NAME: &str = "catalog";
/// The suffix of a segment file's name.
pub const SEGMENT_SUFFIX: &str = ".rec";
/// The suffix of a time index's name.
pub const INDEX_SUFFIX: &str = ".idx";
/// The length of the catalog header and of the index header.
pub const HEADER_LENGTH: usize = 128;
/// The length of one descriptor slot.
pub const DESCRIPTOR_LENGTH: usize = 1024;
/// The length of one time index entry.
pub const ENTRY_LENGTH: usize = 16;
/// The catalog header's magic.
pub const CATALOG_MAGIC: [u8; 8] = *b"CVLYCTLG";
/// The time index header's magic.
pub const INDEX_MAGIC: [u8; 8] = *b"CVLYTIDX";
/// This version of the layouts.
pub const VERSION: i32 = 0;
/// The catalog's bound: slots per directory, fixed by this version (1 GiB of slots).
pub const MAX_SLOTS: u64 = 1 << 20;
/// The longest channel a descriptor holds.
pub const CHANNEL_LENGTH: usize = 256;
/// The longest source identity a descriptor holds.
pub const SOURCE_IDENTITY_LENGTH: usize = 128;
/// The segment file length of this version, for the 64 MiB IPC term.
pub const SEGMENT_FILE_LENGTH: i32 = 128 * 1024 * 1024;
/// A position is a multiple of this (`spec/log-buffer.md`, "Positions").
pub const FRAME_ALIGNMENT: i64 = 32;
const MIN_TERM_LENGTH: i32 = 64 * 1024;
const MAX_TERM_LENGTH: i32 = 1024 * 1024 * 1024;
const MIN_MTU: i32 = 64;
const MAX_MTU: i32 = 65_504;
const DATA_CHECKSUM_NONE: i32 = 0;

/// The zlib CRC32 (polynomial `EDB88320`, reflected) of `bytes`: every language's
/// standard library has it, which is why the catalog uses it (ADR 0024).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Why bytes were rejected, named as `spec/durable-log.md`, "What a reader rejects".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// Fewer bytes than a header, or the file is shorter than one.
    TooShort {
        /// The bytes given.
        length: u64,
    },
    /// The magic is eight zero bytes: the creator has not finished.
    NotReady,
    /// The magic is another file's or garbage.
    Magic,
    /// The version is not [`VERSION`].
    Version {
        /// The version found.
        found: i32,
    },
    /// The catalog's descriptor length is not [`DESCRIPTOR_LENGTH`].
    DescriptorLength {
        /// The value found.
        found: i32,
    },
    /// The catalog's data checksum kind is not 0.
    ChecksumKind {
        /// The value found.
        found: i32,
    },
    /// A timestamp is below 0, or a stop timestamp is before its start.
    Timestamp,
    /// The catalog holds more whole slots than the bound.
    Length {
        /// The file's length.
        length: u64,
    },
    /// A descriptor's state is 2: the driver found the recording damaged.
    Invalid,
    /// A descriptor's state is not 0 to 3.
    State {
        /// The value found.
        found: i32,
    },
    /// A descriptor's creation checksum is not the CRC32 of bytes 28 to 1024: damage.
    CreationChecksum,
    /// A descriptor's stop checksum is not the CRC32 of bytes 8 to 24: a stop in
    /// progress, or damage.
    StopChecksum,
    /// A recording id is not its slot's index, or is negative.
    RecordingId {
        /// The value found.
        found: i64,
    },
    /// A position is negative, unaligned, or a stop position below its start.
    Position {
        /// The value found.
        found: i64,
    },
    /// The term length is not a power of two in range.
    TermLength {
        /// The value found.
        found: i32,
    },
    /// The segment file length is not a power of two of at least the term length.
    SegmentLength {
        /// The value found.
        found: i32,
    },
    /// The MTU length is not a multiple of 32 in range.
    Mtu {
        /// The value found.
        found: i32,
    },
    /// A string's length is over its field, or its bytes are not UTF-8.
    String,
    /// An index entry's timestamp or position is below the entry before it.
    Order,
    /// The frame at a position does not carry the position's term id and offset and the
    /// descriptor's session and stream ids.
    Frame,
}

impl LayoutError {
    /// The spec's name for the check that failed.
    pub const fn reason(&self) -> &'static str {
        match self {
            LayoutError::TooShort { .. } => "too-short",
            LayoutError::NotReady => "not-ready",
            LayoutError::Magic => "magic",
            LayoutError::Version { .. } => "version",
            LayoutError::DescriptorLength { .. } => "descriptor-length",
            LayoutError::ChecksumKind { .. } => "checksum-kind",
            LayoutError::Timestamp => "timestamp",
            LayoutError::Length { .. } => "length",
            LayoutError::Invalid => "invalid",
            LayoutError::State { .. } => "state",
            LayoutError::CreationChecksum | LayoutError::StopChecksum => "checksum",
            LayoutError::RecordingId { .. } => "recording-id",
            LayoutError::Position { .. } => "position",
            LayoutError::TermLength { .. } => "term-length",
            LayoutError::SegmentLength { .. } => "segment-length",
            LayoutError::Mtu { .. } => "mtu",
            LayoutError::String => "string",
            LayoutError::Order => "order",
            LayoutError::Frame => "frame",
        }
    }
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DOC: &str = "docs/reference/durable-log.md#what-a-reader-rejects";
        let reason = self.reason();
        match self {
            LayoutError::TooShort { length } => write!(
                f,
                "durable log rejected ({reason}): {length} bytes, fewer than a header; a catalog \
                 or index torn while the driver created it is written again when the driver \
                 opens the log directory, so start the driver; see {DOC}"
            ),
            LayoutError::NotReady => write!(
                f,
                "durable log not ready ({reason}): the header's magic is zero, the driver has not \
                 finished creating it; try again; see {DOC}"
            ),
            LayoutError::Version { found } => write!(
                f,
                "durable log rejected ({reason}): version {found}, and this reader knows version \
                 {VERSION}; use the version of cavalry that wrote it; see {DOC}"
            ),
            LayoutError::Length { length } => write!(
                f,
                "durable log rejected ({reason}): a file of {length} bytes is not a header plus \
                 whole slots or entries; the file is truncated or was not written by the driver; \
                 see {DOC}"
            ),
            LayoutError::Invalid => write!(
                f,
                "durable log rejected ({reason}): the driver found this recording damaged when it \
                 started; `cavalry doctor` names it and what it found; see {DOC}"
            ),
            LayoutError::CreationChecksum => write!(
                f,
                "durable log rejected ({reason}): a descriptor's creation checksum does not match \
                 its fields, which nothing rewrites after creation: damage, which the driver marks \
                 invalid when it starts and `cavalry doctor` names; see {DOC}"
            ),
            LayoutError::StopChecksum => write!(
                f,
                "durable log rejected ({reason}): a descriptor's stop checksum does not match its \
                 stop fields after 100 ms of retries: a stop torn by a crash, which the driver \
                 repairs from the files when it starts or stops the recording; see {DOC}"
            ),
            LayoutError::Frame => write!(
                f,
                "durable log rejected ({reason}): the frame at a position does not carry the \
                 position's term id and offset and the recording's session and stream ids; the \
                 segment is corrupt or belongs to another recording; see {DOC}"
            ),
            other => write!(
                f,
                "durable log rejected ({reason}): {other:?}; the file was not written by the \
                 driver, or by another version; see {DOC}"
            ),
        }
    }
}

impl std::error::Error for LayoutError {}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn i64_at(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("eight bytes"))
}

fn check_magic(bytes: &[u8], magic: &[u8; 8]) -> Result<(), LayoutError> {
    if bytes[..8] == [0u8; 8] {
        return Err(LayoutError::NotReady);
    }
    if bytes[..8] != magic[..] {
        return Err(LayoutError::Magic);
    }
    Ok(())
}

fn check_version(found: i32) -> Result<(), LayoutError> {
    if found != VERSION {
        return Err(LayoutError::Version { found });
    }
    Ok(())
}

/// The catalog header's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogHeader {
    created: i64,
}

impl CatalogHeader {
    /// A header for a catalog created at `created`, nanoseconds since the Unix epoch.
    pub fn new(created: i64) -> Result<Self, LayoutError> {
        if created < 0 {
            return Err(LayoutError::Timestamp);
        }
        Ok(CatalogHeader { created })
    }

    /// When the catalog was created.
    pub const fn created(&self) -> i64 {
        self.created
    }

    /// The header's bytes, magic first.
    pub fn encode(&self) -> [u8; HEADER_LENGTH] {
        let mut out = [0u8; HEADER_LENGTH];
        out[..8].copy_from_slice(&CATALOG_MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[12..16].copy_from_slice(&(DESCRIPTOR_LENGTH as i32).to_le_bytes());
        out[20..24].copy_from_slice(&DATA_CHECKSUM_NONE.to_le_bytes());
        out[24..32].copy_from_slice(&self.created.to_le_bytes());
        out
    }

    /// Parses a catalog header from its bytes and the file's length, running the page's
    /// checks in order.
    pub fn parse(bytes: &[u8], file_length: u64) -> Result<Self, LayoutError> {
        if bytes.len() < HEADER_LENGTH || file_length < HEADER_LENGTH as u64 {
            return Err(LayoutError::TooShort {
                length: (bytes.len() as u64).min(file_length),
            });
        }
        check_magic(bytes, &CATALOG_MAGIC)?;
        check_version(i32_at(bytes, 8))?;
        let descriptor_length = i32_at(bytes, 12);
        if descriptor_length != DESCRIPTOR_LENGTH as i32 {
            return Err(LayoutError::DescriptorLength {
                found: descriptor_length,
            });
        }
        let kind = i32_at(bytes, 20);
        if kind != DATA_CHECKSUM_NONE {
            return Err(LayoutError::ChecksumKind { found: kind });
        }
        let created = i64_at(bytes, 24);
        if created < 0 {
            return Err(LayoutError::Timestamp);
        }
        if Self::slots(file_length) > MAX_SLOTS {
            return Err(LayoutError::Length {
                length: file_length,
            });
        }
        Ok(CatalogHeader { created })
    }

    /// How many whole slots a catalog of `file_length` bytes holds; bytes past the last
    /// whole slot are ignored, as the page says.
    pub const fn slots(file_length: u64) -> u64 {
        (file_length - HEADER_LENGTH as u64) / DESCRIPTOR_LENGTH as u64
    }

    /// The offset of slot `id`.
    pub const fn slot_offset(id: u64) -> u64 {
        HEADER_LENGTH as u64 + id * DESCRIPTOR_LENGTH as u64
    }
}

/// A descriptor's state: the slot's commit word. Its transitions are the driver's and
/// counted: a slot taken in `recording.started`, a slot set to invalid in
/// `recording.invalidated` (`counters.toml`, the `catalog_slot` state machine; ADR 0024;
/// CLAUDE.md, invariant 4). The delete of card 95 brings its own.
// state machine: catalog_slot
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum State {
    /// The slot's other bytes are not read.
    Empty = 0,
    /// A recording, live or stopped.
    Valid = 1,
    /// A recording a reader found inconsistent; `cavalry doctor` names it.
    Invalid = 2,
    /// A recording whose files are gone; the slot stays so the id is never reused.
    Deleted = 3,
}

impl State {
    /// The state for a stored value.
    pub const fn from_i32(value: i32) -> Option<State> {
        match value {
            0 => Some(State::Empty),
            1 => Some(State::Valid),
            2 => Some(State::Invalid),
            3 => Some(State::Deleted),
            _ => None,
        }
    }
}

/// A descriptor's fields: one recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    state: State,
    recording_id: i64,
    start_timestamp: i64,
    stop_timestamp: i64,
    start_position: i64,
    stop_position: i64,
    initial_term_id: i32,
    segment_file_length: i32,
    term_length: i32,
    mtu_length: i32,
    session_id: i32,
    stream_id: i32,
    channel: String,
    source_identity: String,
}

/// What a new recording's descriptor is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording<'a> {
    /// The slot, which is the recording id.
    pub recording_id: i64,
    /// When the recording started, nanoseconds since the Unix epoch.
    pub start_timestamp: i64,
    /// The stream position the recording starts at.
    pub start_position: i64,
    /// The log buffer's initial term id.
    pub initial_term_id: i32,
    /// The log buffer's term length.
    pub term_length: i32,
    /// The log buffer's MTU length.
    pub mtu_length: i32,
    /// The publication's session id.
    pub session_id: i32,
    /// The publication's stream id.
    pub stream_id: i32,
    /// The channel.
    pub channel: &'a str,
    /// The publisher's identity.
    pub source_identity: &'a str,
}

fn is_power_of_two(value: i32) -> bool {
    value > 0 && (value & (value - 1)) == 0
}

impl Descriptor {
    /// A live recording's descriptor: stop fields at −1, state valid.
    pub fn new(recording: &Recording<'_>) -> Result<Self, LayoutError> {
        let descriptor = Descriptor {
            state: State::Valid,
            recording_id: recording.recording_id,
            start_timestamp: recording.start_timestamp,
            stop_timestamp: -1,
            start_position: recording.start_position,
            stop_position: -1,
            initial_term_id: recording.initial_term_id,
            segment_file_length: SEGMENT_FILE_LENGTH,
            term_length: recording.term_length,
            mtu_length: recording.mtu_length,
            session_id: recording.session_id,
            stream_id: recording.stream_id,
            channel: recording.channel.to_string(),
            source_identity: recording.source_identity.to_string(),
        };
        descriptor.check(recording.recording_id)?;
        Ok(descriptor)
    }

    /// The state.
    pub const fn state(&self) -> State {
        self.state
    }
    /// The recording id, the slot's index.
    pub const fn recording_id(&self) -> i64 {
        self.recording_id
    }
    /// When the recording started.
    pub const fn start_timestamp(&self) -> i64 {
        self.start_timestamp
    }
    /// When it stopped, or −1 while live.
    pub const fn stop_timestamp(&self) -> i64 {
        self.stop_timestamp
    }
    /// The position it starts at.
    pub const fn start_position(&self) -> i64 {
        self.start_position
    }
    /// The position it stopped at, or −1 while live.
    pub const fn stop_position(&self) -> i64 {
        self.stop_position
    }
    /// The log buffer's initial term id.
    pub const fn initial_term_id(&self) -> i32 {
        self.initial_term_id
    }
    /// The segment file length.
    pub const fn segment_file_length(&self) -> i32 {
        self.segment_file_length
    }
    /// The term length.
    pub const fn term_length(&self) -> i32 {
        self.term_length
    }
    /// The MTU length.
    pub const fn mtu_length(&self) -> i32 {
        self.mtu_length
    }
    /// The session id.
    pub const fn session_id(&self) -> i32 {
        self.session_id
    }
    /// The stream id.
    pub const fn stream_id(&self) -> i32 {
        self.stream_id
    }
    /// The channel.
    pub fn channel(&self) -> &str {
        &self.channel
    }
    /// The source identity.
    pub fn source_identity(&self) -> &str {
        &self.source_identity
    }
    /// Whether the recording is live: its stop position is −1.
    pub const fn is_live(&self) -> bool {
        self.stop_position == -1
    }

    /// The descriptor with its stop fields set.
    pub fn stopped(&self, stop_position: i64, stop_timestamp: i64) -> Result<Self, LayoutError> {
        let mut stopped = self.clone();
        stopped.stop_position = stop_position;
        stopped.stop_timestamp = stop_timestamp;
        stopped.check(self.recording_id)?;
        Ok(stopped)
    }

    /// The descriptor with another state.
    pub fn with_state(&self, state: State) -> Self {
        let mut changed = self.clone();
        changed.state = state;
        changed
    }

    /// Every field check, in the page's order: the numeric fields, then the strings.
    fn check(&self, slot: i64) -> Result<(), LayoutError> {
        self.check_fields(slot)?;
        if self.channel.len() > CHANNEL_LENGTH
            || self.source_identity.len() > SOURCE_IDENTITY_LENGTH
        {
            return Err(LayoutError::String);
        }
        Ok(())
    }

    /// The checks after the checksums and before `string`, in the page's order:
    /// `timestamp`, `recording-id`, `position`, `term-length`, `segment-length`, `mtu`.
    fn check_fields(&self, slot: i64) -> Result<(), LayoutError> {
        if self.start_timestamp < 0
            || (self.stop_timestamp != -1 && self.stop_timestamp < self.start_timestamp)
        {
            return Err(LayoutError::Timestamp);
        }
        if self.recording_id != slot || self.recording_id < 0 {
            return Err(LayoutError::RecordingId {
                found: self.recording_id,
            });
        }
        if self.start_position < 0 || self.start_position % FRAME_ALIGNMENT != 0 {
            return Err(LayoutError::Position {
                found: self.start_position,
            });
        }
        if self.stop_position != -1
            && (self.stop_position < self.start_position
                || self.stop_position % FRAME_ALIGNMENT != 0)
        {
            return Err(LayoutError::Position {
                found: self.stop_position,
            });
        }
        if (self.stop_timestamp == -1) != (self.stop_position == -1) {
            return Err(LayoutError::Position {
                found: self.stop_position,
            });
        }
        if !is_power_of_two(self.term_length)
            || self.term_length < MIN_TERM_LENGTH
            || self.term_length > MAX_TERM_LENGTH
        {
            return Err(LayoutError::TermLength {
                found: self.term_length,
            });
        }
        if !is_power_of_two(self.segment_file_length)
            || self.segment_file_length < self.term_length
            || self.segment_file_length > MAX_TERM_LENGTH
        {
            return Err(LayoutError::SegmentLength {
                found: self.segment_file_length,
            });
        }
        if self.mtu_length % 32 != 0 || self.mtu_length < MIN_MTU || self.mtu_length > MAX_MTU {
            return Err(LayoutError::Mtu {
                found: self.mtu_length,
            });
        }
        Ok(())
    }

    /// The slot's bytes 28 to 1024: the creation fields, what the creation checksum
    /// covers, the reserved word before them included.
    fn creation(&self) -> [u8; DESCRIPTOR_LENGTH - 28] {
        let mut c = [0u8; DESCRIPTOR_LENGTH - 28];
        c[4..12].copy_from_slice(&self.recording_id.to_le_bytes());
        c[12..20].copy_from_slice(&self.start_timestamp.to_le_bytes());
        c[20..28].copy_from_slice(&self.start_position.to_le_bytes());
        c[28..32].copy_from_slice(&self.initial_term_id.to_le_bytes());
        c[32..36].copy_from_slice(&self.segment_file_length.to_le_bytes());
        c[36..40].copy_from_slice(&self.term_length.to_le_bytes());
        c[40..44].copy_from_slice(&self.mtu_length.to_le_bytes());
        c[44..48].copy_from_slice(&self.session_id.to_le_bytes());
        c[48..52].copy_from_slice(&self.stream_id.to_le_bytes());
        c[52..56].copy_from_slice(&(self.channel.len() as i32).to_le_bytes());
        c[56..56 + self.channel.len()].copy_from_slice(self.channel.as_bytes());
        c[312..316].copy_from_slice(&(self.source_identity.len() as i32).to_le_bytes());
        c[316..316 + self.source_identity.len()].copy_from_slice(self.source_identity.as_bytes());
        c
    }

    /// The slot's bytes 8 to 24: the stop fields, what the stop checksum covers.
    fn stop_fields(&self) -> [u8; 16] {
        let mut s = [0u8; 16];
        s[..8].copy_from_slice(&self.stop_timestamp.to_le_bytes());
        s[8..].copy_from_slice(&self.stop_position.to_le_bytes());
        s
    }

    /// The slot's bytes: state, stop checksum, stop fields, creation checksum, the
    /// creation fields.
    pub fn encode(&self) -> [u8; DESCRIPTOR_LENGTH] {
        let creation = self.creation();
        let stop = self.stop_fields();
        let mut out = [0u8; DESCRIPTOR_LENGTH];
        out[0..4].copy_from_slice(&(self.state as i32).to_le_bytes());
        out[4..8].copy_from_slice(&crc32(&stop).to_le_bytes());
        out[8..24].copy_from_slice(&stop);
        out[24..28].copy_from_slice(&crc32(&creation).to_le_bytes());
        out[28..].copy_from_slice(&creation);
        out
    }

    /// Parses slot `slot`'s bytes, running the page's checks in order: `state`,
    /// `invalid`, `checksum` (creation, then stop), `timestamp`, `recording-id`,
    /// `position`, `term-length`, `segment-length`, `mtu`, `string`. An empty slot
    /// parses to `None`, and nothing after its state is read.
    pub fn parse(bytes: &[u8], slot: i64) -> Result<Option<Self>, LayoutError> {
        Self::parse_inner(bytes, slot, false).map(|parsed| parsed.map(|(descriptor, _)| descriptor))
    }

    /// Parses a descriptor for the recovery pass (`spec/durable-log.md`, "The driver"):
    /// as [`parse`](Self::parse), except that a stop checksum that does not match is not
    /// a failure, since rebuilding a torn stop is what recovery does. The descriptor then
    /// reads as live, its stop fields at −1, and the flag says the stop was torn.
    pub fn parse_for_recovery(
        bytes: &[u8],
        slot: i64,
    ) -> Result<Option<(Self, bool)>, LayoutError> {
        Self::parse_inner(bytes, slot, true)
    }

    fn parse_inner(
        bytes: &[u8],
        slot: i64,
        tolerate_torn_stop: bool,
    ) -> Result<Option<(Self, bool)>, LayoutError> {
        if bytes.len() < DESCRIPTOR_LENGTH {
            return Err(LayoutError::TooShort {
                length: bytes.len() as u64,
            });
        }
        let state_value = i32_at(bytes, 0);
        let state =
            State::from_i32(state_value).ok_or(LayoutError::State { found: state_value })?;
        if state == State::Empty {
            return Ok(None);
        }
        if state == State::Invalid {
            return Err(LayoutError::Invalid);
        }
        if u32_at(bytes, 24) != crc32(&bytes[28..DESCRIPTOR_LENGTH]) {
            return Err(LayoutError::CreationChecksum);
        }
        let torn_stop = u32_at(bytes, 4) != crc32(&bytes[8..24]);
        if torn_stop && !tolerate_torn_stop {
            return Err(LayoutError::StopChecksum);
        }
        let mut descriptor = Descriptor {
            state,
            stop_timestamp: if torn_stop { -1 } else { i64_at(bytes, 8) },
            stop_position: if torn_stop { -1 } else { i64_at(bytes, 16) },
            recording_id: i64_at(bytes, 32),
            start_timestamp: i64_at(bytes, 40),
            start_position: i64_at(bytes, 48),
            initial_term_id: i32_at(bytes, 56),
            segment_file_length: i32_at(bytes, 60),
            term_length: i32_at(bytes, 64),
            mtu_length: i32_at(bytes, 68),
            session_id: i32_at(bytes, 72),
            stream_id: i32_at(bytes, 76),
            channel: String::new(),
            source_identity: String::new(),
        };
        descriptor.check_fields(slot)?;
        let channel_length = i32_at(bytes, 80);
        let source_length = i32_at(bytes, 340);
        if channel_length < 0
            || channel_length as usize > CHANNEL_LENGTH
            || source_length < 0
            || source_length as usize > SOURCE_IDENTITY_LENGTH
        {
            return Err(LayoutError::String);
        }
        descriptor.channel = std::str::from_utf8(&bytes[84..84 + channel_length as usize])
            .map_err(|_| LayoutError::String)?
            .to_string();
        descriptor.source_identity = std::str::from_utf8(&bytes[344..344 + source_length as usize])
            .map_err(|_| LayoutError::String)?
            .to_string();
        Ok(Some((descriptor, torn_stop)))
    }
}

/// The time index header's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHeader {
    recording_id: i64,
}

impl IndexHeader {
    /// The header of recording `recording_id`'s index.
    pub fn new(recording_id: i64) -> Result<Self, LayoutError> {
        if recording_id < 0 {
            return Err(LayoutError::RecordingId {
                found: recording_id,
            });
        }
        Ok(IndexHeader { recording_id })
    }

    /// The recording this index belongs to.
    pub const fn recording_id(&self) -> i64 {
        self.recording_id
    }

    /// The header's bytes, magic first.
    pub fn encode(&self) -> [u8; HEADER_LENGTH] {
        let mut out = [0u8; HEADER_LENGTH];
        out[..8].copy_from_slice(&INDEX_MAGIC);
        out[8..12].copy_from_slice(&VERSION.to_le_bytes());
        out[16..24].copy_from_slice(&self.recording_id.to_le_bytes());
        out
    }

    /// Parses an index header from its bytes and the file's length.
    pub fn parse(bytes: &[u8], file_length: u64) -> Result<Self, LayoutError> {
        if bytes.len() < HEADER_LENGTH || file_length < HEADER_LENGTH as u64 {
            return Err(LayoutError::TooShort {
                length: (bytes.len() as u64).min(file_length),
            });
        }
        check_magic(bytes, &INDEX_MAGIC)?;
        check_version(i32_at(bytes, 8))?;
        let recording_id = i64_at(bytes, 16);
        if recording_id < 0 {
            return Err(LayoutError::RecordingId {
                found: recording_id,
            });
        }
        Ok(IndexHeader { recording_id })
    }

    /// How many whole entries an index of `file_length` bytes holds; a part-written
    /// last entry is ignored, as the page says.
    pub const fn entries(file_length: u64) -> u64 {
        (file_length - HEADER_LENGTH as u64) / ENTRY_LENGTH as u64
    }
}

/// One time index entry: the position the recording had reached at an instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexEntry {
    /// Nanoseconds since the Unix epoch.
    pub timestamp: i64,
    /// The position, a multiple of 32.
    pub position: i64,
}

impl IndexEntry {
    /// The entry's bytes.
    pub fn encode(&self) -> [u8; ENTRY_LENGTH] {
        let mut out = [0u8; ENTRY_LENGTH];
        out[..8].copy_from_slice(&self.timestamp.to_le_bytes());
        out[8..].copy_from_slice(&self.position.to_le_bytes());
        out
    }

    /// Parses an entry, checked against the entry before it when there is one.
    pub fn parse(bytes: &[u8], previous: Option<&IndexEntry>) -> Result<Self, LayoutError> {
        if bytes.len() < ENTRY_LENGTH {
            return Err(LayoutError::TooShort {
                length: bytes.len() as u64,
            });
        }
        let entry = IndexEntry {
            timestamp: i64_at(bytes, 0),
            position: i64_at(bytes, 8),
        };
        if entry.timestamp < 0 {
            return Err(LayoutError::Timestamp);
        }
        if entry.position < 0 || entry.position % FRAME_ALIGNMENT != 0 {
            return Err(LayoutError::Position {
                found: entry.position,
            });
        }
        if let Some(previous) = previous
            && (entry.timestamp < previous.timestamp || entry.position < previous.position)
        {
            return Err(LayoutError::Order);
        }
        Ok(entry)
    }
}

/// Where a position's bytes are: the segment file and the offset in it, and what the
/// frame header there must say (`spec/durable-log.md`, "Segment files").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The segment file's name.
    pub segment_file: String,
    /// The segment's base position.
    pub segment_base_position: i64,
    /// The position's offset in the segment.
    pub offset_in_segment: i64,
    /// The term id the frame header must carry.
    pub term_id: i32,
    /// The term offset the frame header must carry.
    pub term_offset: i32,
}

/// Locates `position` in recording `descriptor`, by Aeron's rule: the first segment
/// starts at the start position's term base, and each next one a segment file length
/// further.
pub fn locate(descriptor: &Descriptor, position: i64) -> Result<Located, LayoutError> {
    if position < descriptor.start_position || position % FRAME_ALIGNMENT != 0 {
        return Err(LayoutError::Position { found: position });
    }
    let term_length = i64::from(descriptor.term_length);
    let segment_length = i64::from(descriptor.segment_file_length);
    let position_bits = term_length.trailing_zeros();
    let term_count = position >> position_bits;
    let term_id = descriptor.initial_term_id.wrapping_add(term_count as i32);
    let start_term_base = descriptor.start_position - descriptor.start_position % term_length;
    let from_base = position - start_term_base;
    let segment_base_position = start_term_base + from_base - from_base % segment_length;
    Ok(Located {
        segment_file: segment_file_name(descriptor.recording_id, segment_base_position),
        segment_base_position,
        offset_in_segment: position - segment_base_position,
        term_id,
        term_offset: (position % term_length) as i32,
    })
}

/// The name of a segment file.
pub fn segment_file_name(recording_id: i64, segment_base_position: i64) -> String {
    format!("{recording_id}-{segment_base_position}{SEGMENT_SUFFIX}")
}

/// The name of a recording's time index.
pub fn index_file_name(recording_id: i64) -> String {
    format!("{recording_id}{INDEX_SUFFIX}")
}
