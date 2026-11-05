//! The calling thread's last error: a code and a message in a fixed buffer, set by every
//! function that fails and read by `cavalry_errcode` and `cavalry_errmsg`
//! (`spec/c-abi.md`, "Error codes" and "The message"). Nothing here allocates: the
//! message is formatted into the buffer, and a message longer than the buffer keeps its
//! head, `...` and its tail from the last `; see `, so the docs anchor survives.

use std::cell::RefCell;
use std::ffi::{c_char, c_int};
use std::fmt::{self, Write};

use cavalry_client::{ClientError, Fault, Offer};

use crate::codes::*;

/// The buffer's size: at most `BUFFER - 1` bytes of message, then the NUL.
pub(crate) const BUFFER: usize = 512;
/// The tail kept while a long message is formatted: its last bytes, in a ring.
const TAIL: usize = 128;
const ELLIPSIS: &[u8] = b"...";
const SEE: &[u8] = b"; see ";

struct State {
    code: c_int,
    /// The message, NUL-terminated inside the buffer.
    buffer: [u8; BUFFER],
}

thread_local! {
    static ERROR: RefCell<State> = const { RefCell::new(State { code: CAVALRY_ERR_NONE, buffer: [0; BUFFER] }) };
}

/// Formats a message into the head and the tail ring without allocating.
struct Writer {
    head: [u8; BUFFER - 1],
    head_len: usize,
    ring: [u8; TAIL],
    /// Bytes written in all; the ring holds the last `min(total, TAIL)` of them, with
    /// the newest at `total % TAIL`.
    total: usize,
}

impl Writer {
    const fn new() -> Self {
        Self {
            head: [0; BUFFER - 1],
            head_len: 0,
            ring: [0; TAIL],
            total: 0,
        }
    }

    /// The last `min(total, TAIL)` bytes, in order, copied out of the ring.
    fn tail(&self, out: &mut [u8; TAIL]) -> usize {
        let len = self.total.min(TAIL);
        let start = self.total % TAIL;
        for (i, slot) in out.iter_mut().enumerate().take(len) {
            *slot = self.ring[(start + TAIL - len + i) % TAIL];
        }
        len
    }

    /// Writes the message, whole or cut, NUL-terminated, into `buffer`.
    fn finish(&self, buffer: &mut [u8; BUFFER]) {
        if self.total < BUFFER {
            buffer[..self.head_len].copy_from_slice(&self.head[..self.head_len]);
            buffer[self.head_len] = 0;
            return;
        }
        let mut last = [0u8; TAIL];
        let last_len = self.tail(&mut last);
        // The tail: from the last `; see ` when that part fits the ring, else the
        // ring's bytes, moved forward to the start of a UTF-8 sequence.
        let mut tail_start = 0;
        if let Some(at) = rfind(&last[..last_len], SEE) {
            tail_start = at;
        }
        while tail_start < last_len && !is_boundary(last[tail_start]) {
            tail_start += 1;
        }
        let tail = &last[tail_start..last_len];
        let mut head_len = (BUFFER - 1).saturating_sub(ELLIPSIS.len() + tail.len());
        head_len = head_len.min(self.head_len);
        while head_len > 0 && !is_boundary(self.head[head_len]) {
            head_len -= 1;
        }
        let mut at = 0;
        buffer[at..at + head_len].copy_from_slice(&self.head[..head_len]);
        at += head_len;
        buffer[at..at + ELLIPSIS.len()].copy_from_slice(ELLIPSIS);
        at += ELLIPSIS.len();
        buffer[at..at + tail.len()].copy_from_slice(tail);
        at += tail.len();
        buffer[at] = 0;
    }
}

/// Whether a byte begins a UTF-8 sequence (or is ASCII).
const fn is_boundary(byte: u8) -> bool {
    byte & 0xC0 != 0x80
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .rev()
        .find(|&i| &haystack[i..i + needle.len()] == needle)
}

impl Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &byte in s.as_bytes() {
            if self.head_len < BUFFER - 1 {
                self.head[self.head_len] = byte;
                self.head_len += 1;
            }
            self.ring[self.total % TAIL] = byte;
            self.total += 1;
        }
        Ok(())
    }
}

/// Sets the thread's error to `code` with the formatted message.
pub(crate) fn set(code: c_int, message: fmt::Arguments<'_>) {
    let mut writer = Writer::new();
    // Formatting into a fixed writer cannot fail.
    let _ = writer.write_fmt(message);
    ERROR.with(|state| {
        let mut state = state.borrow_mut();
        state.code = code;
        writer.finish(&mut state.buffer);
    });
}

/// Sets the thread's error from a client error: the driver's code on its value for a
/// refusal from 1 to 99, else the client's kind.
pub(crate) fn set_client(error: &ClientError) {
    let code = match error {
        ClientError::NotFound { .. } => CAVALRY_ERR_NOT_FOUND,
        ClientError::DriverGone { .. } => CAVALRY_ERR_DRIVER_GONE,
        ClientError::Closed(_) => CAVALRY_ERR_CLOSED,
        ClientError::Refused { raw_code, .. } => {
            if (1..=99).contains(raw_code) {
                *raw_code
            } else {
                CAVALRY_ERR_REFUSED
            }
        }
        ClientError::SubscriptionsFull { .. } => CAVALRY_ERR_SUBSCRIPTIONS_FULL,
        ClientError::ReplaysFull { .. } => CAVALRY_ERR_REPLAYS_FULL,
        ClientError::ReplayQueueFull { .. } => CAVALRY_ERR_REPLAY_QUEUE_FULL,
        ClientError::ResponseTimeout { .. } => CAVALRY_ERR_RESPONSE_TIMEOUT,
        ClientError::CommandRing(_) => CAVALRY_ERR_COMMAND_RING,
        ClientError::Broadcast(_) => CAVALRY_ERR_BROADCAST,
        ClientError::Counters(_) => CAVALRY_ERR_COUNTERS,
        ClientError::Protocol(_) => CAVALRY_ERR_PROTOCOL,
        ClientError::Log(_) => CAVALRY_ERR_LOG,
        ClientError::Driver { .. } => CAVALRY_ERR_DRIVER,
        ClientError::Io { .. } => CAVALRY_ERR_IO,
    };
    set(code, format_args!("{error}"));
}

/// Sets the thread's error from a poll's fault and returns its code.
pub(crate) fn set_fault(fault: &Fault) -> c_int {
    let code = match fault {
        Fault::Image { .. } => CAVALRY_ERR_FAULT_IMAGE,
        Fault::ImagesFull { .. } => CAVALRY_ERR_FAULT_IMAGES_FULL,
        Fault::Closed => CAVALRY_ERR_FAULT_CLOSED,
    };
    set(code, format_args!("{fault}"));
    code
}

/// The result code of a claim or an offer refusal; `TooLong` sets the thread's error.
pub(crate) fn offer_code(offer: &Offer) -> i64 {
    match offer {
        Offer::NotConnected => CAVALRY_PUBLICATION_NOT_CONNECTED,
        Offer::BackPressured => CAVALRY_PUBLICATION_BACK_PRESSURED,
        Offer::AdminAction => CAVALRY_PUBLICATION_ADMIN_ACTION,
        Offer::MaxPositionExceeded => CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED,
        Offer::Closed => CAVALRY_PUBLICATION_CLOSED,
        Offer::TooLong { .. } => {
            set(CAVALRY_ERR_TOO_LONG, format_args!("{offer}"));
            CAVALRY_PUBLICATION_ERROR
        }
    }
}

/// The docs anchor the ABI's own messages end with.
pub(crate) const DOC: &str = "docs/reference/c-abi.md";

/// A null handle or a null required pointer, in `function`.
pub(crate) fn null(function: &str, what: &str) {
    set(
        CAVALRY_ERR_NULL,
        format_args!(
            "{function} refused (null): {what} is null; pass the handle or the pointer the \
             call needs; see {DOC}#when-a-function-cannot-answer"
        ),
    );
}

/// The calling thread's last error code; 0 with no failure yet.
#[unsafe(no_mangle)]
pub extern "C" fn cavalry_errcode() -> c_int {
    ERROR.with(|state| state.borrow().code)
}

/// The calling thread's last error message, UTF-8 and NUL-terminated, valid until the
/// next failing call on this thread; the empty string with no failure yet.
#[unsafe(no_mangle)]
pub extern "C" fn cavalry_errmsg() -> *const c_char {
    ERROR.with(|state| state.borrow().buffer.as_ptr().cast::<c_char>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> Vec<u8> {
        ERROR.with(|state| {
            let state = state.borrow();
            let end = state.buffer.iter().position(|&b| b == 0).expect("a NUL");
            state.buffer[..end].to_vec()
        })
    }

    #[test]
    fn a_short_message_is_written_whole() {
        set(7, format_args!("short; see {DOC}"));
        assert_eq!(cavalry_errcode(), 7);
        assert_eq!(message(), format!("short; see {DOC}").into_bytes());
    }

    #[test]
    fn a_long_message_keeps_its_anchor_and_stays_utf8() {
        let what = "é".repeat(3000);
        set(
            112,
            format_args!("client refused (driver): the driver answered with {what}; see {DOC}#x"),
        );
        let bytes = message();
        assert!(bytes.len() < BUFFER);
        let text = std::str::from_utf8(&bytes).expect("valid UTF-8");
        assert!(text.starts_with("client refused (driver): the driver answered with é"));
        assert!(text.contains("..."), "{text}");
        assert!(text.ends_with(&format!("; see {DOC}#x")), "{text}");
    }

    /// The bytes `cavalry_errmsg` points at up to its NUL, which must be inside the
    /// buffer, as a C caller reads them.
    fn errmsg_bytes() -> Vec<u8> {
        // SAFETY: `cavalry_errmsg` points at this thread's buffer of `BUFFER` bytes,
        // valid until the next failing call on the thread, and none runs here.
        let buffer = unsafe { std::slice::from_raw_parts(cavalry_errmsg().cast::<u8>(), BUFFER) };
        let end = buffer
            .iter()
            .position(|&b| b == 0)
            .expect("the NUL is inside the 512 bytes");
        buffer[..end].to_vec()
    }

    #[test]
    fn a_long_error_message_keeps_its_docs_anchor() {
        use std::path::PathBuf;

        // The three messages ADR 0051 names ("The message"), formatted by the crates
        // that write them, each with the anchor its own Display puts last.
        // 4,096 bytes, ending in three-byte characters.
        let what = format!("a{}", "€".repeat(1365));
        // 4,096 bytes, none of them ASCII.
        let path = "é".repeat(2048);
        assert_eq!((what.len(), path.len()), (4096, 4096));
        let driver = ClientError::Driver { what };
        let io = ClientError::Io {
            what: "open the counters file",
            path: PathBuf::from(path),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        let too_long = Offer::TooLong {
            found: usize::MAX,
            limit: usize::MAX,
        };

        set_client(&driver);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_DRIVER);
        let anchor = "; see docs/reference/client-protocol.md#error-codes";
        check(&driver.to_string(), anchor);

        set_client(&io);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_IO);
        let anchor = "; see docs/reference/client-protocol.md#the-driver-directory";
        check(&io.to_string(), anchor);

        assert_eq!(offer_code(&too_long), CAVALRY_PUBLICATION_ERROR);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_TOO_LONG);
        let anchor = "; see docs/reference/log-buffer.md#claim-commit-and-abort";
        check(&too_long.to_string(), anchor);

        /// The message in the buffer is valid UTF-8, ends with the error's own anchor,
        /// and is the whole message when that fits, else its head, `...` and its tail.
        fn check(whole: &str, anchor: &str) {
            assert!(whole.ends_with(anchor), "the error's Display ends {anchor}");
            let bytes = errmsg_bytes();
            assert!(bytes.len() < BUFFER);
            let text = std::str::from_utf8(&bytes).expect("valid UTF-8 before the NUL");
            assert!(text.ends_with(anchor), "{text}");
            if whole.len() < BUFFER {
                assert_eq!(text, whole);
            } else {
                let (head, _) = text.split_once("...").expect("a cut message holds `...`");
                assert!(whole.starts_with(head), "{text}");
                assert!(head.len() > BUFFER / 2, "the head keeps most of the buffer");
            }
        }
    }

    #[test]
    fn a_long_message_with_no_anchor_keeps_its_last_bytes() {
        let what = "ü".repeat(600);
        set(1, format_args!("{what}END"));
        let bytes = message();
        let text = std::str::from_utf8(&bytes).expect("valid UTF-8");
        assert!(text.ends_with("üEND"), "{text}");
        assert!(bytes.len() < BUFFER);
    }
}
