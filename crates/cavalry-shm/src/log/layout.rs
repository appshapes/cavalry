//! The bytes and the arithmetic of `spec/log-buffer.md`, version 0, on plain values: the
//! constants, the header encoding, the tail packing, the position arithmetic, the claim
//! arithmetic and the claim header's bytes, with the reader checks in the spec's order.
//! No `unsafe` and no mapping here, so Miri runs every test that uses this module and
//! the spec's vectors run against it; `cells` and `file` put the same functions over a
//! mapping.

use cavalry_proto::frame::{
    DataFrameHeader, FRAME_ALIGNMENT, Flags, FrameType, HEADER_LENGTH as FRAME_HEADER_LENGTH,
};
use zerocopy::little_endian::{I32, I64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::LogError;

/// Length of the metadata page in bytes.
pub const META_DATA_LENGTH: usize = 4096;
/// Length of the header, the first bytes of the metadata page.
pub const HEADER_LENGTH: usize = 128;
/// How many term buffers a log has.
pub const PARTITION_COUNT: usize = 3;
/// The first eight bytes of a log buffer's metadata page, `CVLYLOGB`, written last.
pub const MAGIC: [u8; 8] = *b"CVLYLOGB";
/// The version of `spec/log-buffer.md` this crate writes; it reads [`MIN_VERSION`] to
/// this one (ADR 0040: version 1 added the image type and the receiver window length).
pub const VERSION: i32 = 1;
/// The oldest version this crate reads: a version 0 log is a version 1 log whose
/// receiver window length is 0.
pub const MIN_VERSION: i32 = 0;
/// The smallest term length, 64 KiB.
pub const MIN_TERM_LENGTH: u32 = 64 * 1024;
/// The largest term length, 1 GiB.
pub const MAX_TERM_LENGTH: u32 = 1024 * 1024 * 1024;
/// The smallest MTU length: one frame header and one aligned unit.
pub const MIN_MTU_LENGTH: u32 = 64;
/// The largest MTU length: the largest UDP payload rounded down to the frame alignment.
pub const MAX_MTU_LENGTH: u32 = 65_504;
/// The longest message any term length allows, 16 MiB.
pub const MAX_MESSAGE_LENGTH_CAP: u32 = 16 * 1024 * 1024;
/// The largest position is the term length shifted by this.
pub const MAX_POSITION_SHIFT: u32 = 31;
/// Offset of the default frame header in the header.
pub const DEFAULT_FRAME_HEADER_OFFSET: usize = 64;
/// Offset of tail counter 0 in the metadata page; the others follow at this stride.
pub const TAIL_COUNTERS_OFFSET: usize = 128;
/// The distance between tail counters: one slot each (ADR 0011).
pub const SLOT_LENGTH: usize = 128;
/// Offset of the active term count in the metadata page.
pub const ACTIVE_TERM_COUNT_OFFSET: usize = 512;
/// Offset of the end of stream position in the metadata page.
pub const END_OF_STREAM_POSITION_OFFSET: usize = 640;
/// Offset of the is connected flag in the metadata page.
pub const IS_CONNECTED_OFFSET: usize = 704;
/// The directory under the driver's that holds publication logs.
pub const FILE_DIRECTORY: &str = "publications";
/// The directory under the driver's that holds image logs, type 2 (ADR 0040).
pub const IMAGE_DIRECTORY: &str = "images";
/// Offset of the receiver window length in the header, version 1.
pub const RECEIVER_WINDOW_LENGTH_OFFSET: usize = 40;
/// The suffix of a log buffer's file name, after the correlation id in decimal.
pub const FILE_SUFFIX: &str = ".logbuffer";
/// The end of stream position of a publication that is open.
pub const NOT_ENDED: i64 = i64::MAX;

const UNFRAGMENTED_FLAGS: u8 = 0xC0;

/// The header's table after the magic, as bytes.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawHeaderRest {
    version: I32,
    log_type: I32,
    correlation_id: I64,
    initial_term_id: I32,
    term_length: I32,
    mtu_length: I32,
    publication_window_length: I32,
    receiver_window_length: I32,
    reserved_a: [u8; 20],
    default_frame_header: [u8; FRAME_HEADER_LENGTH],
    reserved_b: [u8; 32],
}

const _: () = assert!(size_of::<RawHeaderRest>() == HEADER_LENGTH - 8);
const _: () = assert!(
    core::mem::offset_of!(RawHeaderRest, default_frame_header) == DEFAULT_FRAME_HEADER_OFFSET - 8
);
const _: () = assert!(
    core::mem::offset_of!(RawHeaderRest, receiver_window_length)
        == RECEIVER_WINDOW_LENGTH_OFFSET - 8
);

/// How a log's terms are written: the header's type field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum LogType {
    /// Any number of publishers advance the tail with fetch-and-add.
    Concurrent = 0,
    /// One publisher keeps the tail itself and stores it with release stores.
    Exclusive = 1,
    /// An image: the driver's receiver writes frames by term offset as they arrive from
    /// the wire, the first eight bytes last; the tails are not advanced and nobody reads
    /// them (`spec/log-buffer.md`, version 1; `spec/udp.md`, "The receiver"; ADR 0040).
    Image = 2,
}

impl LogType {
    /// Whether this log's terms are written by the receiver, not by publishers.
    pub const fn is_image(self) -> bool {
        matches!(self, LogType::Image)
    }
}

impl TryFrom<i32> for LogType {
    type Error = LogError;

    fn try_from(value: i32) -> Result<Self, LogError> {
        match value {
            0 => Ok(LogType::Concurrent),
            1 => Ok(LogType::Exclusive),
            2 => Ok(LogType::Image),
            found => Err(LogError::Type { found }),
        }
    }
}

/// The header's fields after the magic, validated: the version [`MIN_VERSION`] to
/// [`VERSION`], the type one of [`LogType`], the term length a power of two from
/// [`MIN_TERM_LENGTH`] to [`MAX_TERM_LENGTH`], the MTU length a multiple of 32 from
/// [`MIN_MTU_LENGTH`] to [`MAX_MTU_LENGTH`], the publication window 1 to term length
/// ÷ 2, the receiver window 1 to term length ÷ 2 in an image and 0 otherwise, and the
/// default frame header what the spec says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeaderFields {
    version: i32,
    log_type: LogType,
    correlation_id: i64,
    initial_term_id: i32,
    term_length: u32,
    mtu_length: u32,
    publication_window_length: u32,
    receiver_window_length: u32,
    session_id: i32,
    stream_id: i32,
}

impl HeaderFields {
    /// Fields for a log of `log_type` for the publication `correlation_id`, session
    /// `session_id` on stream `stream_id`, with terms of `term_length` bytes starting at
    /// `initial_term_id`, frames of at most `mtu_length` bytes and a window of
    /// `publication_window_length` bytes. Runs the spec's term-length, mtu and window
    /// checks, in that order.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        log_type: LogType,
        correlation_id: i64,
        initial_term_id: i32,
        term_length: u32,
        mtu_length: u32,
        publication_window_length: u32,
        session_id: i32,
        stream_id: i32,
    ) -> Result<Self, LogError> {
        if !(MIN_TERM_LENGTH..=MAX_TERM_LENGTH).contains(&term_length)
            || !term_length.is_power_of_two()
        {
            return Err(LogError::TermLength {
                found: i64::from(term_length),
            });
        }
        if !(MIN_MTU_LENGTH..=MAX_MTU_LENGTH).contains(&mtu_length)
            || !mtu_length.is_multiple_of(FRAME_ALIGNMENT as u32)
        {
            return Err(LogError::Mtu {
                found: i64::from(mtu_length),
            });
        }
        if !(1..=term_length / 2).contains(&publication_window_length) {
            return Err(LogError::Window {
                found: i64::from(publication_window_length),
                term_length,
            });
        }
        if log_type.is_image() {
            return Err(LogError::ReceiverWindow {
                found: 0,
                term_length,
            });
        }
        Ok(Self {
            version: VERSION,
            log_type,
            correlation_id,
            initial_term_id,
            term_length,
            mtu_length,
            publication_window_length,
            receiver_window_length: 0,
            session_id,
            stream_id,
        })
    }

    /// Fields for an image, type 2 (`spec/log-buffer.md`, version 1; ADR 0040): the log
    /// the driver's receiver rebuilds for the publication `correlation_id` is the
    /// image's own registration id, session `session_id` on stream `stream_id`, with the
    /// sender's term length, MTU length and initial term id, a publication window of
    /// term length ÷ 2 that nobody reads, and `receiver_window_length` from 1 to term
    /// length ÷ 2, the `window` check.
    pub fn new_image(
        correlation_id: i64,
        initial_term_id: i32,
        term_length: u32,
        mtu_length: u32,
        receiver_window_length: u32,
        session_id: i32,
        stream_id: i32,
    ) -> Result<Self, LogError> {
        let mut fields = Self::new(
            LogType::Exclusive,
            correlation_id,
            initial_term_id,
            term_length,
            mtu_length,
            (term_length / 2).max(1),
            session_id,
            stream_id,
        )?;
        if !(1..=term_length / 2).contains(&receiver_window_length) {
            return Err(LogError::ReceiverWindow {
                found: i64::from(receiver_window_length),
                term_length,
            });
        }
        fields.log_type = LogType::Image;
        fields.receiver_window_length = receiver_window_length;
        Ok(fields)
    }

    /// The same fields with the version `version`, [`MIN_VERSION`] to [`VERSION`]:
    /// what a reader of an older log holds, and what a test encodes an older vector
    /// from; a writer writes [`VERSION`]. [`LogError::Version`] for any other.
    pub fn with_version(mut self, version: i32) -> Result<Self, LogError> {
        if !(MIN_VERSION..=VERSION).contains(&version) {
            return Err(LogError::Version { found: version });
        }
        self.version = version;
        Ok(self)
    }

    /// The version the header carries: [`VERSION`] for a log this crate created, or an
    /// older one it read.
    pub const fn version(&self) -> i32 {
        self.version
    }

    /// How far past the consumption position the sender may send into an image: 1 to
    /// term length ÷ 2; 0 in a publication's log (`spec/udp.md`, "The receiver").
    pub const fn receiver_window_length(&self) -> u32 {
        self.receiver_window_length
    }

    /// How the tail is advanced.
    pub const fn log_type(&self) -> LogType {
        self.log_type
    }

    /// The publication's registration with the driver, and the file's name.
    pub const fn correlation_id(&self) -> i64 {
        self.correlation_id
    }

    /// The term id of term count 0.
    pub const fn initial_term_id(&self) -> i32 {
        self.initial_term_id
    }

    /// The length of each term in bytes.
    pub const fn term_length(&self) -> u32 {
        self.term_length
    }

    /// The longest frame a message is split into.
    pub const fn mtu_length(&self) -> u32 {
        self.mtu_length
    }

    /// How far the publisher may run ahead of the slowest subscriber.
    pub const fn publication_window_length(&self) -> u32 {
        self.publication_window_length
    }

    /// The publication's session id, from the default frame header.
    pub const fn session_id(&self) -> i32 {
        self.session_id
    }

    /// The publication's stream id, from the default frame header.
    pub const fn stream_id(&self) -> i32 {
        self.stream_id
    }

    /// The longest body one frame carries: MTU length − 32.
    pub const fn max_payload_length(&self) -> u32 {
        self.mtu_length - FRAME_HEADER_LENGTH as u32
    }

    /// The longest message: the smaller of term length ÷ 8 and 16 MiB.
    pub const fn max_message_length(&self) -> u32 {
        let eighth = self.term_length / 8;
        if eighth < MAX_MESSAGE_LENGTH_CAP {
            eighth
        } else {
            MAX_MESSAGE_LENGTH_CAP
        }
    }

    /// The file length these fields imply: 3 × term length + 4096.
    pub const fn file_length(&self) -> u64 {
        PARTITION_COUNT as u64 * self.term_length as u64 + META_DATA_LENGTH as u64
    }

    /// Where the metadata page begins in the file: 3 × term length.
    pub const fn meta_data_offset(&self) -> u64 {
        PARTITION_COUNT as u64 * self.term_length as u64
    }

    /// Where term buffer `partition` begins in the file, for a partition below
    /// [`PARTITION_COUNT`].
    pub const fn term_buffer_offset(&self, partition: usize) -> u64 {
        partition as u64 * self.term_length as u64
    }

    /// The position arithmetic for this log.
    pub const fn positions(&self) -> Positions {
        Positions::new(self.initial_term_id, self.term_length)
    }

    /// The 32 bytes of the default frame header: frame length 0, version 0, flags `C0`,
    /// type 1, term offset 0, the session and stream ids, the initial term id and
    /// reserved value 0.
    pub fn default_frame_header(&self) -> [u8; FRAME_HEADER_LENGTH] {
        let mut bytes =
            DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, FRAME_HEADER_LENGTH)
                .unwrap_or_else(|_| unreachable!("a header-length frame is valid"))
                .with_session_id(self.session_id)
                .with_stream_id(self.stream_id)
                .with_term_id(self.initial_term_id)
                .to_bytes();
        bytes[..4].copy_from_slice(&[0; 4]);
        bytes
    }

    /// The file's name inside the driver's directory: `publications/<correlation
    /// id>.logbuffer`, or `images/<correlation id>.logbuffer` for an image.
    pub fn file_name(&self) -> std::path::PathBuf {
        if self.log_type.is_image() {
            image_file_name(self.correlation_id)
        } else {
            file_name(self.correlation_id)
        }
    }
}

/// The file name of the log of publication `correlation_id`: `publications/<id>.logbuffer`.
pub fn file_name(correlation_id: i64) -> std::path::PathBuf {
    std::path::PathBuf::from(FILE_DIRECTORY).join(format!("{correlation_id}{FILE_SUFFIX}"))
}

/// The file name of the log of image `correlation_id`: `images/<id>.logbuffer` (ADR 0040).
pub fn image_file_name(correlation_id: i64) -> std::path::PathBuf {
    std::path::PathBuf::from(IMAGE_DIRECTORY).join(format!("{correlation_id}{FILE_SUFFIX}"))
}

/// Which directory a log's file name names, and its correlation id: the inverse of
/// [`file_name`] and [`image_file_name`] for the name an `available image` or
/// `publication ready` event carries, which a client checks before it opens anything
/// (ADR 0010). `None` for any other string.
pub fn parse_file_name(name: &[u8]) -> Option<(LogType, i64)> {
    let name = core::str::from_utf8(name).ok()?;
    let (directory, rest) = name.split_once('/')?;
    let log_type = match directory {
        FILE_DIRECTORY => LogType::Concurrent,
        IMAGE_DIRECTORY => LogType::Image,
        _ => return None,
    };
    let id = rest.strip_suffix(FILE_SUFFIX)?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some((log_type, id.parse().ok()?))
}

/// The 128 bytes of a header for these fields, magic included.
pub fn encode_header(fields: &HeaderFields) -> [u8; HEADER_LENGTH] {
    let mut out = [0u8; HEADER_LENGTH];
    out[..8].copy_from_slice(&MAGIC);
    out[8..].copy_from_slice(&encode_header_rest(fields));
    out
}

/// The header's bytes after the magic.
pub(crate) fn encode_header_rest(fields: &HeaderFields) -> [u8; HEADER_LENGTH - 8] {
    let raw = RawHeaderRest {
        version: I32::new(fields.version),
        log_type: I32::new(fields.log_type as i32),
        correlation_id: I64::new(fields.correlation_id),
        initial_term_id: I32::new(fields.initial_term_id),
        term_length: I32::new(fields.term_length as i32),
        mtu_length: I32::new(fields.mtu_length as i32),
        publication_window_length: I32::new(fields.publication_window_length as i32),
        receiver_window_length: I32::new(fields.receiver_window_length as i32),
        reserved_a: [0; 20],
        default_frame_header: fields.default_frame_header(),
        reserved_b: [0; 32],
    };
    zerocopy::transmute!(raw)
}

/// Reads a header from the front of `bytes` for a file of `file_length` bytes and runs
/// the spec's checks in order: too-short, not-ready, magic, version, type, term-length,
/// mtu, window, frame-header, length. The `changed` check needs two reads and lives in
/// the file layer.
pub fn parse_header(bytes: &[u8], file_length: u64) -> Result<HeaderFields, LogError> {
    if bytes.len() < HEADER_LENGTH {
        return Err(LogError::TooShort {
            given: bytes.len() as u64,
        });
    }
    if file_length < META_DATA_LENGTH as u64 {
        return Err(LogError::TooShort { given: file_length });
    }
    let mut magic = [0u8; 8];
    magic.copy_from_slice(&bytes[..8]);
    check_magic(magic)?;
    let mut rest = [0u8; HEADER_LENGTH - 8];
    rest.copy_from_slice(&bytes[8..HEADER_LENGTH]);
    parse_header_rest(&rest, file_length)
}

/// The not-ready and magic checks on the first eight bytes.
pub(crate) fn check_magic(magic: [u8; 8]) -> Result<(), LogError> {
    if magic == [0; 8] {
        return Err(LogError::NotReady);
    }
    if magic != MAGIC {
        return Err(LogError::Magic { found: magic });
    }
    Ok(())
}

/// The version, type, term-length, mtu, window, frame-header and length checks on the
/// bytes after the magic.
pub(crate) fn parse_header_rest(
    rest: &[u8; HEADER_LENGTH - 8],
    file_length: u64,
) -> Result<HeaderFields, LogError> {
    let Ok(raw) = RawHeaderRest::ref_from_bytes(rest) else {
        return Err(LogError::TooShort {
            given: rest.len() as u64,
        });
    };
    let version = raw.version.get();
    if !(MIN_VERSION..=VERSION).contains(&version) {
        return Err(LogError::Version { found: version });
    }
    let log_type = LogType::try_from(raw.log_type.get())?;
    let term_length = raw.term_length.get();
    let term_length = u32::try_from(term_length).map_err(|_| LogError::TermLength {
        found: i64::from(term_length),
    })?;
    if !(MIN_TERM_LENGTH..=MAX_TERM_LENGTH).contains(&term_length) || !term_length.is_power_of_two()
    {
        return Err(LogError::TermLength {
            found: i64::from(term_length),
        });
    }
    let mtu_length = raw.mtu_length.get();
    let mtu_length = u32::try_from(mtu_length).map_err(|_| LogError::Mtu {
        found: i64::from(mtu_length),
    })?;
    let window = raw.publication_window_length.get();
    let window = u32::try_from(window).map_err(|_| LogError::Window {
        found: i64::from(window),
        term_length,
    })?;
    let header = &raw.default_frame_header;
    let initial_term_id = raw.initial_term_id.get();
    let frame_header_ok = header[..4] == [0; 4]
        && header[4] == 0
        && header[5] == UNFRAGMENTED_FLAGS
        && header[6..8] == (FrameType::Data as u16).to_le_bytes()
        && header[8..12] == [0; 4]
        && header[20..24] == initial_term_id.to_le_bytes()
        && header[24..32] == [0; 8];
    let session_id = i32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    let stream_id = i32::from_le_bytes([header[16], header[17], header[18], header[19]]);
    let receiver_window = raw.receiver_window_length.get();
    let fields = if log_type.is_image() {
        let receiver_window =
            u32::try_from(receiver_window).map_err(|_| LogError::ReceiverWindow {
                found: i64::from(receiver_window),
                term_length,
            })?;
        // The publication window is checked by `new_image` through `new`; the receiver
        // window after it, as the spec orders the two.
        let _ = HeaderFields::new(
            LogType::Exclusive,
            raw.correlation_id.get(),
            initial_term_id,
            term_length,
            mtu_length,
            window,
            session_id,
            stream_id,
        )?;
        let mut fields = HeaderFields::new_image(
            raw.correlation_id.get(),
            initial_term_id,
            term_length,
            mtu_length,
            receiver_window,
            session_id,
            stream_id,
        )?;
        fields.publication_window_length = window;
        fields
    } else {
        if receiver_window != 0 {
            return Err(LogError::ReceiverWindow {
                found: i64::from(receiver_window),
                term_length,
            });
        }
        HeaderFields::new(
            log_type,
            raw.correlation_id.get(),
            initial_term_id,
            term_length,
            mtu_length,
            window,
            session_id,
            stream_id,
        )?
    };
    let fields = fields.with_version(version)?;
    if !frame_header_ok {
        return Err(LogError::FrameHeader);
    }
    let needed = fields.file_length();
    if file_length != needed {
        return Err(LogError::Length {
            found: file_length,
            needed,
        });
    }
    Ok(fields)
}

/// A tail counter: a term id in the high 32 bits, a term offset in the low 32.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tail(pub i64);

impl Tail {
    /// The tail for `term_id` at `term_offset`.
    pub const fn pack(term_id: i32, term_offset: u32) -> Tail {
        Tail(((term_id as i64) << 32) | term_offset as i64)
    }

    /// The term id in the high 32 bits.
    pub const fn term_id(self) -> i32 {
        (self.0 >> 32) as i32
    }

    /// The low 32 bits as they are, which a claim that ran past the end leaves above
    /// the term length.
    pub const fn raw_term_offset(self) -> u32 {
        self.0 as u32
    }

    /// The term offset: the low 32 bits, or `term_length` when they exceed it.
    pub const fn term_offset(self, term_length: u32) -> u32 {
        let raw = self.raw_term_offset();
        if raw < term_length { raw } else { term_length }
    }
}

/// The position arithmetic of one log: initial term id and term length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Positions {
    initial_term_id: i32,
    term_length: u32,
    bits: u32,
}

impl Positions {
    /// For a log whose first term is `initial_term_id` and whose terms are `term_length`
    /// bytes, a power of two.
    pub const fn new(initial_term_id: i32, term_length: u32) -> Self {
        Self {
            initial_term_id,
            term_length,
            bits: term_length.trailing_zeros(),
        }
    }

    /// The term count of `term_id`: term id − initial term id in wrapping 32-bit
    /// arithmetic, so a term id that wrapped still counts upward.
    pub const fn term_count(&self, term_id: i32) -> i32 {
        term_id.wrapping_sub(self.initial_term_id)
    }

    /// The position of `term_offset` in term `term_id`.
    pub const fn position(&self, term_id: i32, term_offset: u32) -> i64 {
        ((self.term_count(term_id) as i64) << self.bits) + term_offset as i64
    }

    /// The position of the start of term `term_id`.
    pub const fn term_begin(&self, term_id: i32) -> i64 {
        (self.term_count(term_id) as i64) << self.bits
    }

    /// The term count a position is in.
    pub const fn term_count_of(&self, position: i64) -> i64 {
        position >> self.bits
    }

    /// The term id a position is in.
    pub const fn term_id_of(&self, position: i64) -> i32 {
        self.initial_term_id
            .wrapping_add(self.term_count_of(position) as i32)
    }

    /// The offset of a position within its term.
    pub const fn term_offset_of(&self, position: i64) -> u32 {
        (position & (self.term_length as i64 - 1)) as u32
    }

    /// The partition a position's term is in: its term count modulo 3.
    pub const fn partition_of(&self, position: i64) -> usize {
        (self.term_count_of(position) % PARTITION_COUNT as i64) as usize
    }

    /// The partition of term count `term_count`.
    pub const fn partition_of_count(term_count: i32) -> usize {
        (term_count as u32 % PARTITION_COUNT as u32) as usize
    }

    /// The largest position: term length × 2^31. A claim that would reach it fails.
    pub const fn max_position(&self) -> i64 {
        (self.term_length as i64) << MAX_POSITION_SHIFT
    }

    /// The term length.
    pub const fn term_length(&self) -> u32 {
        self.term_length
    }

    /// The initial term id.
    pub const fn initial_term_id(&self) -> i32 {
        self.initial_term_id
    }
}

/// Why a claim was refused: the spec's four results that are not a claim. Each is
/// returned to the caller, which decides what to do; nothing is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaimRefused {
    /// No subscriber has a position on this log, and the position is at the limit.
    NotConnected,
    /// The slowest subscriber is a window behind: try again later.
    BackPressured,
    /// A rotation happened, or another publisher's did: try again at once.
    AdminAction,
    /// The stream reached the largest position its term length allows: close the
    /// publication and create a new one.
    MaxPositionExceeded,
}

impl ClaimRefused {
    /// The result's name: `not-connected`, `back-pressured`, `admin-action` or
    /// `max-position-exceeded`.
    pub const fn reason(self) -> &'static str {
        match self {
            ClaimRefused::NotConnected => "not-connected",
            ClaimRefused::BackPressured => "back-pressured",
            ClaimRefused::AdminAction => "admin-action",
            ClaimRefused::MaxPositionExceeded => "max-position-exceeded",
        }
    }
}

/// The frame length rounded up to a multiple of 32.
pub const fn aligned_length(frame_length: u32) -> u32 {
    frame_length.div_ceil(FRAME_ALIGNMENT as u32) * FRAME_ALIGNMENT as u32
}

/// Step 1 of a claim: with the active term count and the active tail loaded, the
/// position the claim starts at, or why it is refused before anything is reserved: the
/// tail's term id is not the active term's (`admin action`), or the position is not
/// below `limit` (`max position exceeded`, `back pressured` or `not connected`).
pub fn before_reserve(
    positions: &Positions,
    active_term_count: i32,
    tail: Tail,
    aligned_length: u32,
    limit: i64,
    is_connected: bool,
) -> Result<i64, ClaimRefused> {
    let term_id = tail.term_id();
    if positions.term_count(term_id) != active_term_count {
        return Err(ClaimRefused::AdminAction);
    }
    let position = positions.position(term_id, tail.term_offset(positions.term_length()));
    if position < limit {
        return Ok(position);
    }
    if position + i64::from(aligned_length) >= positions.max_position() {
        Err(ClaimRefused::MaxPositionExceeded)
    } else if is_connected {
        Err(ClaimRefused::BackPressured)
    } else {
        Err(ClaimRefused::NotConnected)
    }
}

/// The padding frame that fills the end of a term when a claim does not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Padding {
    /// Where the padding frame begins.
    pub term_offset: u32,
    /// Its frame length: the bytes to the end of the term.
    pub frame_length: u32,
}

/// What rotating out of a full term does to the metadata page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rotation {
    /// The partition rotated into.
    pub next_partition: usize,
    /// The term id the next partition's tail must still hold for the rotation to write
    /// it: the term three back.
    pub expected_term_id: i32,
    /// The tail the next partition gets: the next term id at offset 0.
    pub next_tail: Tail,
    /// The active term count after the rotation.
    pub next_term_count: i32,
}

/// Steps 2 and 3 of a claim, after the reservation: where the frame goes, or what
/// happens at the end of the term.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reserved {
    /// The frame fits at `term_offset` in term `term_id`; the position after it is
    /// `position_after`.
    Fits {
        /// The term the frame is in.
        term_id: i32,
        /// Where the frame begins.
        term_offset: u32,
        /// The stream position after the frame.
        position_after: i64,
    },
    /// The frame does not fit. The padding frame, if the tail was still inside the
    /// term, fills the rest, the rotation moves the stream on, and the caller gets
    /// `admin action`.
    EndOfTerm {
        /// The term that is full.
        term_id: i32,
        /// The padding frame to write, or none when the tail was already at or past the
        /// end.
        padding: Option<Padding>,
        /// How to rotate.
        rotation: Rotation,
    },
    /// The frame does not fit and the position after it would reach the largest
    /// position. The padding frame, if any, is still written; the caller gets `max
    /// position exceeded`.
    MaxPositionExceeded {
        /// The term that is full.
        term_id: i32,
        /// The padding frame to write, or none.
        padding: Option<Padding>,
    },
}

/// Steps 2 and 3 of a claim: `tail_before` is the tail the reservation returned, that
/// is the tail before `aligned_length` was added.
pub fn after_reserve(
    positions: &Positions,
    active_term_count: i32,
    tail_before: Tail,
    aligned_length: u32,
) -> Reserved {
    let term_length = positions.term_length();
    let term_id = tail_before.term_id();
    let term_offset = tail_before.term_offset(term_length);
    let resulting = u64::from(term_offset) + u64::from(aligned_length);
    if resulting <= u64::from(term_length) {
        return Reserved::Fits {
            term_id,
            term_offset,
            position_after: positions.position(term_id, term_offset) + i64::from(aligned_length),
        };
    }
    let padding = (term_offset < term_length).then_some(Padding {
        term_offset,
        frame_length: term_length - term_offset,
    });
    let position_after = positions.term_begin(term_id) + resulting as i64;
    if position_after >= positions.max_position() {
        return Reserved::MaxPositionExceeded { term_id, padding };
    }
    let next_term_id = term_id.wrapping_add(1);
    let next_term_count = active_term_count.wrapping_add(1);
    Reserved::EndOfTerm {
        term_id,
        padding,
        rotation: Rotation {
            next_partition: Positions::partition_of_count(next_term_count),
            expected_term_id: next_term_id.wrapping_sub(PARTITION_COUNT as i32),
            next_tail: Tail::pack(next_term_id, 0),
            next_term_count,
        },
    }
}

/// The 32 bytes a publisher writes when it claims (`spec/log-buffer.md`, step 4): the
/// frame length negated, the version, flags and type of the default frame header, the
/// term offset, the session and stream ids from the default frame header, the term id
/// and the reserved value.
pub fn claim_header(
    default_frame_header: &[u8; FRAME_HEADER_LENGTH],
    term_offset: u32,
    frame_length: u32,
    term_id: i32,
    reserved_value: i64,
) -> [u8; FRAME_HEADER_LENGTH] {
    let mut bytes = *default_frame_header;
    bytes[..4].copy_from_slice(&(frame_length as i32).wrapping_neg().to_le_bytes());
    bytes[8..12].copy_from_slice(&(term_offset as i32).to_le_bytes());
    bytes[20..24].copy_from_slice(&term_id.to_le_bytes());
    bytes[24..32].copy_from_slice(&reserved_value.to_le_bytes());
    bytes
}

/// The same 32 bytes after commit: the positive frame length.
pub fn committed_header(claimed: &[u8; FRAME_HEADER_LENGTH]) -> [u8; FRAME_HEADER_LENGTH] {
    let mut bytes = *claimed;
    let length = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).wrapping_neg();
    bytes[..4].copy_from_slice(&length.to_le_bytes());
    bytes
}

/// The same 32 bytes after abort: type 0 and the positive frame length.
pub fn aborted_header(claimed: &[u8; FRAME_HEADER_LENGTH]) -> [u8; FRAME_HEADER_LENGTH] {
    let mut bytes = committed_header(claimed);
    bytes[6..8].copy_from_slice(&(FrameType::Pad as u16).to_le_bytes());
    bytes
}

/// The first eight bytes of a frame as the word the claim protocol stores: the frame
/// length in the low 32 bits, version, flags and type in the high 32.
pub const fn frame_word(header: &[u8; FRAME_HEADER_LENGTH]) -> u64 {
    u64::from_le_bytes([
        header[0], header[1], header[2], header[3], header[4], header[5], header[6], header[7],
    ])
}

/// The frame length in a frame word: its low 32 bits as a signed integer.
pub const fn frame_word_length(word: u64) -> i32 {
    word as u32 as i32
}

/// The frame word with its length replaced.
pub const fn frame_word_with_length(word: u64, frame_length: i32) -> u64 {
    (word & 0xFFFF_FFFF_0000_0000) | (frame_length as u32 as u64)
}

/// The frame word with its type replaced by `frame_type`.
pub const fn frame_word_with_type(word: u64, frame_type: FrameType) -> u64 {
    (word & 0x0000_FFFF_FFFF_FFFF) | ((frame_type as u16 as u64) << 48)
}
