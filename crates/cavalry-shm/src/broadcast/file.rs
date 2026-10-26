//! The broadcast buffer on disk: creating it, mapping it, the driver's transmitter and
//! a client's receiver, each over the cells of `cells`. Every `unsafe` block that
//! touches a mapping is in this module; `cells` and `layout` never see a file.

use core::fmt;
use std::fs::OpenOptions;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use memmap2::{Mmap, MmapMut, MmapOptions};

use crate::pretouch;

use super::BroadcastError;
use super::cells::{self, Received, ReceiverState, TrailerCells};
use super::layout::{self, FILE_NAME, HEADER_LENGTH, HeaderFields, Positions, TRAILER_LENGTH};
use crate::ring::{Words, now_nanos};
use crate::sync::AtomicU64;

/// The writable mapping of the driver, or the read-only mapping of a client or
/// `doctor`.
enum Map {
    Writer(MmapMut),
    Reader(Mmap),
}

/// A mapping with the header fields it was checked against. Every cell is a view at an
/// offset of `base`, and every `unsafe` block below relies on the same facts: the
/// mapping is exactly `fields.file_length()` bytes, checked in `new`; a word index is a
/// multiple of 8 below the capacity, which the arithmetic of `layout` derives from
/// positions masked by capacity − 1 and lengths it checked; the base is page-aligned, so
/// a multiple of 8 keeps the words' alignment and the capacity, a multiple of 4096,
/// keeps the trailer's 128; and the cells are atomics and reserved bytes, valid for
/// every bit pattern, which the driver writes only through the protocol `cells` names.
struct Mapped {
    _map: Map,
    base: *const u8,
    length: usize,
    fields: HeaderFields,
    writable: bool,
    /// Whether this process created the buffer: only the creator, the driver, transmits.
    created: bool,
    /// Whether a [`Transmitter`] on this mapping is alive: the trailer has one writer,
    /// and two would interleave records and corrupt the buffer for every receiver.
    transmitter_taken: AtomicBool,
}

// SAFETY: `base` points into `_map`, which lives as long as `self`, and the only
// accesses through it are the atomics of `cells`; the pointer itself is never written
// after `new`.
unsafe impl Send for Mapped {}
// SAFETY: as above.
unsafe impl Sync for Mapped {}

impl Mapped {
    fn new(mut map: Map, fields: HeaderFields, created: bool) -> Result<Self, BroadcastError> {
        // The writer's pointer comes from `&mut [u8]`, so it keeps permission to write;
        // the map is never dereferenced again, so nothing invalidates it.
        let (base, length, writable) = match &mut map {
            Map::Writer(map) => (map.as_mut_ptr().cast_const(), map.len(), true),
            Map::Reader(map) => (map.as_ptr(), map.len(), false),
        };
        let needed = fields.file_length();
        if length as u64 != needed {
            return Err(BroadcastError::Length {
                found: length as u64,
                needed,
            });
        }
        Ok(Self {
            _map: map,
            base,
            length,
            fields,
            writable,
            created,
            transmitter_taken: AtomicBool::new(false),
        })
    }

    fn trailer(&self) -> &TrailerCells {
        let offset = self.fields.trailer_offset() as usize;
        debug_assert!(offset + TRAILER_LENGTH == self.length);
        // SAFETY: the mapping is exactly `file_length()` bytes (`new`), so the 512 bytes
        // at the capacity are the mapping's last 512; the capacity is a multiple of
        // 4096 from a page-aligned base, which the trailer's 128-byte alignment needs;
        // `TrailerCells` is atomics and reserved bytes; the reference lives no longer
        // than `self`, which owns the map.
        unsafe { &*self.base.add(offset).cast::<TrailerCells>() }
    }

    fn require_writable(&self) -> Result<(), BroadcastError> {
        if self.writable {
            Ok(())
        } else {
            Err(BroadcastError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the broadcast buffer was opened read-only; the driver opens it writable to transmit",
            )))
        }
    }
}

impl Words for Mapped {
    fn capacity(&self) -> u32 {
        self.fields.capacity()
    }

    fn word(&self, index: u32) -> &AtomicU64 {
        debug_assert!(index.is_multiple_of(8) && index < self.fields.capacity());
        // SAFETY: `index` is a multiple of 8 below the capacity (the arithmetic of
        // `layout` masks every position by capacity − 1 and bounds every length it
        // adds; asserted above in debug builds), so the word is inside the ring, which
        // is inside the mapping, at the 8-byte alignment an `AtomicU64` needs from a
        // page-aligned base; an atomic is valid for every bit pattern; the reference
        // lives no longer than `self`. A receiver's mapping is read-only and it only
        // loads; the kernel refuses a store through it.
        unsafe { &*self.base.add(index as usize).cast::<AtomicU64>() }
    }
}

/// One broadcast buffer: the mapping the driver created or a client opened. `Send` and
/// `Sync`: a [`Transmitter`] or a [`Receiver`] made from it keeps the mapping alive on
/// its own.
pub struct BroadcastBuffer {
    mapped: Arc<Mapped>,
}

impl BroadcastBuffer {
    /// Creates `dir/broadcast-buffer` for a buffer of `capacity` bytes and publishes it:
    /// sizes the file and writes the header magic-last (`spec/broadcast-buffer.md`,
    /// "Trailer"). The capacity is a power of two from [`layout::MIN_CAPACITY`] to
    /// [`layout::MAX_CAPACITY`], or the error is [`BroadcastError::Capacity`]; a file
    /// already at that path is [`BroadcastError::Io`].
    pub fn create(dir: &Path, capacity: u32) -> Result<Self, BroadcastError> {
        let fields = HeaderFields::new(capacity, i64::from(std::process::id()), now_nanos())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.join(FILE_NAME))?;
        let length = fields.file_length();
        file.set_len(length)?;
        // SAFETY: memmap2 marks mapping a file `unsafe` because another process could
        // truncate it under the mapping and turn a load into a fault. This call created
        // the file, in the driver's directory, the trust boundary every shared layout
        // assumes (ADR 0010 bounds the values a peer writes, not whether the file
        // exists); a truncation is a fault, never silent misbehaviour.
        let map = unsafe { MmapOptions::new().len(length as usize).map_mut(&file)? };
        pretouch::writable(&map)?;
        let mapped = Arc::new(Mapped::new(Map::Writer(map), fields, true)?);
        mapped.trailer().publish(&fields);
        Ok(Self { mapped })
    }

    /// Opens `dir/broadcast-buffer`, read-only for a client or `doctor`, and runs the spec's checks in the spec's order
    /// (`spec/broadcast-buffer.md`, "What a reader rejects"): the file's size
    /// (`too-short`), the header read once from the file at its last 512 bytes
    /// (`not-ready` to `length`), then the header again through the final mapping, which
    /// must equal the first read (`changed`). A buffer opened `writable` still has no
    /// transmitter: only the one [`BroadcastBuffer::create`] returned transmits.
    pub fn open(dir: &Path, writable: bool) -> Result<Self, BroadcastError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(dir.join(FILE_NAME))?;
        let file_length = file.metadata()?.len();
        if file_length < TRAILER_LENGTH as u64 {
            return Err(BroadcastError::TooShort { given: file_length });
        }
        let mut probe = [0u8; HEADER_LENGTH];
        file.seek(SeekFrom::Start(file_length - TRAILER_LENGTH as u64))?;
        file.read_exact(&mut probe)?;
        let first = layout::parse_header(&probe, file_length)?;
        // SAFETY: as at `create`: memmap2 marks the mapping `unsafe` because another
        // process could truncate the file underneath it, and the driver's directory is
        // the trust boundary; a truncation faults, never lies.
        let map = if writable {
            let map = unsafe {
                MmapOptions::new()
                    .len(file_length as usize)
                    .map_mut(&file)?
            };
            pretouch::writable(&map)?;
            Map::Writer(map)
        } else {
            // SAFETY: as above.
            let map = unsafe { MmapOptions::new().len(file_length as usize).map(&file)? };
            pretouch::readable(&map)?;
            Map::Reader(map)
        };
        let mapped = Mapped::new(map, first, false)?;
        let second = mapped.trailer().read(file_length)?;
        if second != first {
            return Err(BroadcastError::Changed);
        }
        Ok(Self {
            mapped: Arc::new(mapped),
        })
    }

    /// The header's fields.
    pub fn header(&self) -> HeaderFields {
        self.mapped.fields
    }

    /// The buffer's capacity in bytes.
    pub fn capacity(&self) -> u32 {
        self.mapped.fields.capacity()
    }

    /// The longest message this buffer carries: capacity ÷ 8.
    pub fn max_message_length(&self) -> usize {
        self.mapped.fields.max_message_length() as usize
    }

    /// The driver's transmitter on this buffer. One per buffer: the trailer has one
    /// writer. So only the buffer this process created with [`BroadcastBuffer::create`]
    /// has a transmitter, and only one at a time: on a buffer from
    /// [`BroadcastBuffer::open`], or while another transmitter on this buffer is alive,
    /// the error is [`BroadcastError::Io`] and says which.
    pub fn transmitter(&self) -> Result<Transmitter, BroadcastError> {
        self.mapped.require_writable()?;
        if !self.mapped.created {
            return Err(BroadcastError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "only the driver that created the broadcast buffer transmits on it; take the \
                 transmitter from the BroadcastBuffer that create returned, and use an opened \
                 buffer for receivers",
            )));
        }
        if self.mapped.transmitter_taken.swap(true, Ordering::AcqRel) {
            return Err(BroadcastError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "the broadcast buffer already has a transmitter and a buffer has one; use that \
                 transmitter, or drop it before taking another",
            )));
        }
        Ok(Transmitter {
            mapped: Arc::clone(&self.mapped),
        })
    }

    /// A receiver on this buffer, joined at the latest record, with a scratch buffer of
    /// the longest message the buffer carries, allocated once here.
    pub fn receiver(&self) -> Result<Receiver, BroadcastError> {
        let state = cells::join(self.mapped.trailer());
        Ok(Receiver {
            mapped: Arc::clone(&self.mapped),
            state,
            scratch: vec![0; self.max_message_length()],
        })
    }

    /// The tail position, the tail intent and the latest record, each with an acquire
    /// load: what `doctor` reports.
    pub fn positions(&self) -> Positions {
        self.mapped.trailer().positions()
    }
}

impl fmt::Debug for BroadcastBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BroadcastBuffer")
            .field("header", &self.mapped.fields)
            .finish()
    }
}

/// The driver's handle on the buffer (`spec/broadcast-buffer.md`, "Transmitting"): it
/// writes records and never waits for a receiver. One per buffer, from the buffer the
/// driver created ([`BroadcastBuffer::transmitter`]); dropping it lets that buffer hand
/// out another. The handle is `Send` and keeps the mapping alive.
pub struct Transmitter {
    mapped: Arc<Mapped>,
}

impl Transmitter {
    /// The longest message this buffer carries: capacity ÷ 8.
    pub fn max_message_length(&self) -> usize {
        self.mapped.fields.max_message_length() as usize
    }

    /// Transmits `message` as one record of type `type_id`. A type below 1 is
    /// [`BroadcastError::Type`] and a message over [`Transmitter::max_message_length`]
    /// is [`BroadcastError::Message`], both before anything is written; otherwise the
    /// record is written, whether or not any receiver has room for it.
    pub fn transmit(&mut self, type_id: i32, message: &[u8]) -> Result<(), BroadcastError> {
        let mapped = &*self.mapped;
        cells::transmit(mapped, mapped.trailer(), type_id, message)
    }

    /// The tail position, the tail intent and the latest record.
    pub fn positions(&self) -> Positions {
        self.mapped.trailer().positions()
    }
}

impl Drop for Transmitter {
    fn drop(&mut self) {
        self.mapped
            .transmitter_taken
            .store(false, Ordering::Release);
    }
}

impl fmt::Debug for Transmitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transmitter")
            .field("positions", &self.positions())
            .finish()
    }
}

/// A client's handle on the buffer (`spec/broadcast-buffer.md`, "Receiving"): it
/// copies each record out, validates the copy against the tail intent, and reports
/// `lapped` when the transmitter overwrote what it had not read. The handle is `Send`
/// and keeps the mapping alive; its scratch buffer is allocated once, in
/// [`BroadcastBuffer::receiver`].
pub struct Receiver {
    mapped: Arc<Mapped>,
    state: ReceiverState,
    scratch: Vec<u8>,
}

impl Receiver {
    /// The position of the record last delivered, or where the receiver joined.
    pub fn cursor(&self) -> i64 {
        self.state.cursor
    }

    /// The position of the next record the receiver will read.
    pub fn next_record(&self) -> i64 {
        self.state.next_record
    }

    /// Receives the next record, if one is available: its type and a copy of its message
    /// go to `handler`, and the result is `true`; `false` when nothing is available. A
    /// lap, before or after the copy, is [`BroadcastError::Lapped`], and the receiver
    /// continues from the latest record on the next call; positions or a record outside
    /// their bounds are [`BroadcastError::Tail`] or [`BroadcastError::Record`], which
    /// mean the file is corrupt or not the driver's.
    pub fn receive<F>(&mut self, mut handler: F) -> Result<bool, BroadcastError>
    where
        F: FnMut(i32, &[u8]),
    {
        let mapped = &*self.mapped;
        match cells::receive(mapped, mapped.trailer(), &mut self.state, &mut self.scratch)? {
            Received::None => Ok(false),
            Received::Lapped {
                cursor,
                tail_intent,
                latest,
            } => Err(BroadcastError::Lapped {
                cursor,
                tail_intent,
                latest,
            }),
            Received::Message { type_id, length } => {
                handler(type_id, &self.scratch[..length]);
                Ok(true)
            }
        }
    }
}

impl fmt::Debug for Receiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Receiver")
            .field("cursor", &self.state.cursor)
            .field("next_record", &self.state.next_record)
            .finish()
    }
}
