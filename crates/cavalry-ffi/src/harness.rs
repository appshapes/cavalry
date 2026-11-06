//! The client path through the ABI for `just bench`'s `ffi` rows (ADR 0051): the calls
//! a C program makes, in the order it makes them, each one `extern "C"` function of
//! this crate behind a safe method, because `bench/` inherits the workspace's ban on
//! `unsafe` (CLAUDE.md, Unsafe) and may not call the functions itself. Nothing here is
//! part of the ABI: the header declares none of it, and a client in another language
//! calls the functions, not this. The per-message path is one method, one `extern "C"`
//! call and the Rust core behind it, so the rows measure the ABI and the allocation
//! gate counts its path as it counts the client's (ADR 0047).

use std::ffi::{CStr, CString, c_void};
use std::path::Path;
use std::ptr;

use crate::client::{
    cavalry_add_publication, cavalry_add_subscription, cavalry_close, cavalry_init, cavalry_t,
};
use crate::codes::{
    CAVALRY_ABI_VERSION, CAVALRY_PUBLICATION_ADMIN_ACTION, CAVALRY_PUBLICATION_BACK_PRESSURED,
    CAVALRY_PUBLICATION_CLOSED, CAVALRY_PUBLICATION_ERROR,
    CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED, CAVALRY_PUBLICATION_NOT_CONNECTED,
};
use crate::context::{
    cavalry_context_close, cavalry_context_init_v, cavalry_context_set_dir, cavalry_context_t,
};
use crate::error::{DOC, cavalry_errcode, cavalry_errmsg};
use crate::publication::{
    cavalry_buffer_claim_commit, cavalry_buffer_claim_t, cavalry_publication_close,
    cavalry_publication_position, cavalry_publication_t, cavalry_publication_try_claim,
};
use crate::subscription::{
    cavalry_header_t, cavalry_subscription_close, cavalry_subscription_is_connected,
    cavalry_subscription_poll, cavalry_subscription_t,
};

/// The thread's last error, as a C program would print it.
fn last_error(function: &str) -> String {
    // SAFETY: the message is NUL-terminated and valid until the next failing call.
    let message = unsafe { CStr::from_ptr(cavalry_errmsg()) }.to_string_lossy();
    format!(
        "{function} failed with code {}: {message}",
        cavalry_errcode()
    )
}

/// What a claim's result `code`, one of `CAVALRY_PUBLICATION_*`, tells its caller, as
/// `Offer`'s message tells the Rust client's: the define, what to do next, the thread's
/// error after `CAVALRY_PUBLICATION_ERROR`, and the page's anchor (`spec/c-abi.md`,
/// "Result codes"). It allocates, so it is for the error path, after a refusal the
/// caller does not retry.
pub fn refusal(code: i64) -> String {
    let (name, next) = match code {
        CAVALRY_PUBLICATION_NOT_CONNECTED => (
            "CAVALRY_PUBLICATION_NOT_CONNECTED",
            "no subscriber has a position on this stream; nothing is lost, try again when \
             one connects",
        ),
        CAVALRY_PUBLICATION_BACK_PRESSURED => (
            "CAVALRY_PUBLICATION_BACK_PRESSURED",
            "the slowest subscriber is a window behind; try again later, and read \
             cavalry_image_position to find it",
        ),
        CAVALRY_PUBLICATION_ADMIN_ACTION => (
            "CAVALRY_PUBLICATION_ADMIN_ACTION",
            "the term rotated; try again at once",
        ),
        CAVALRY_PUBLICATION_CLOSED => (
            "CAVALRY_PUBLICATION_CLOSED",
            "the client is closed, and cavalry_closed says why; connect a new client and add \
             the publication again",
        ),
        CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED => (
            "CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED",
            "the stream reached the largest position its term length allows; close the \
             publication and add a new one",
        ),
        CAVALRY_PUBLICATION_ERROR => {
            // SAFETY: the message is NUL-terminated and valid until the next failing
            // call, and none runs before it is copied here.
            let message = unsafe { CStr::from_ptr(cavalry_errmsg()) }.to_string_lossy();
            return format!(
                "claim refused (CAVALRY_PUBLICATION_ERROR, {code}): the thread's error, code \
                 {}, says what happened and what to do next: {message}; see \
                 {DOC}#result-codes",
                cavalry_errcode()
            );
        }
        _ => (
            "a value spec/c-abi.md does not define",
            "a claim returns a position or one of the six; report this message as a bug in \
             cavalry-ffi",
        ),
    };
    format!("claim refused ({name}, {code}): {next}; see {DOC}#result-codes")
}

/// A client made through the ABI: a context and a client handle.
pub struct Client {
    context: *mut cavalry_context_t,
    client: *mut cavalry_t,
}

/// A publication handle, used from one thread at a time, as the page says.
pub struct Publication(*mut cavalry_publication_t);

/// A subscription handle, used from one thread at a time, as the page says.
pub struct Subscription(*mut cavalry_subscription_t);

// SAFETY: a handle may move between threads between calls ("Threads"), and each
// wrapper is the one holder of its handle.
unsafe impl Send for Publication {}
// SAFETY: as for `Publication`.
unsafe impl Send for Subscription {}

impl Client {
    /// Attaches to the driver at `dir`, through `cavalry_context_init_v`,
    /// `cavalry_context_set_dir` and `cavalry_init`.
    pub fn connect(dir: &Path) -> Result<Self, String> {
        let dir_c = CString::new(dir.to_str().ok_or("the directory is not UTF-8")?)
            .map_err(|e| e.to_string())?;
        let mut context = ptr::null_mut();
        // SAFETY: the pointers are this function's, and the directory string is
        // NUL-terminated and read during the call.
        unsafe {
            if cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION) != 0 {
                return Err(last_error("cavalry_context_init"));
            }
            if cavalry_context_set_dir(context, dir_c.as_ptr()) != 0 {
                let error = last_error("cavalry_context_set_dir");
                cavalry_context_close(context);
                return Err(error);
            }
            let mut client = ptr::null_mut();
            if cavalry_init(&mut client, context) != 0 {
                let error = last_error("cavalry_init");
                cavalry_context_close(context);
                return Err(error);
            }
            Ok(Client { context, client })
        }
    }

    /// `cavalry_add_publication`.
    pub fn add_publication(
        &mut self,
        channel: &str,
        stream_id: i32,
    ) -> Result<Publication, String> {
        let channel = CString::new(channel).map_err(|e| e.to_string())?;
        let mut publication = ptr::null_mut();
        // SAFETY: a live client handle used from this thread, and a NUL-terminated channel.
        if unsafe {
            cavalry_add_publication(&mut publication, self.client, channel.as_ptr(), stream_id)
        } != 0
        {
            return Err(last_error("cavalry_add_publication"));
        }
        Ok(Publication(publication))
    }

    /// `cavalry_add_subscription`.
    pub fn add_subscription(
        &mut self,
        channel: &str,
        stream_id: i32,
    ) -> Result<Subscription, String> {
        let channel = CString::new(channel).map_err(|e| e.to_string())?;
        let mut subscription = ptr::null_mut();
        // SAFETY: as for `add_publication`.
        if unsafe {
            cavalry_add_subscription(&mut subscription, self.client, channel.as_ptr(), stream_id)
        } != 0
        {
            return Err(last_error("cavalry_add_subscription"));
        }
        Ok(Subscription(subscription))
    }

    /// `cavalry_close`, then the context's close.
    pub fn close(self) -> Result<(), String> {
        // SAFETY: each handle is closed once, here; the wrapper is consumed.
        let closed = unsafe { cavalry_close(self.client) };
        let error = (closed != 0).then(|| last_error("cavalry_close"));
        // SAFETY: as above.
        unsafe { cavalry_context_close(self.context) };
        std::mem::forget(self);
        error.map_or(Ok(()), Err)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: a wrapper dropped without `close` closes its handles once, here.
        unsafe {
            cavalry_close(self.client);
            cavalry_context_close(self.context);
        }
    }
}

impl Publication {
    /// One message of `body` bytes with `stamp` in its first eight, by
    /// `cavalry_publication_try_claim` and `cavalry_buffer_claim_commit`: the position
    /// after the frame, or the claim's result code, `CAVALRY_PUBLICATION_*`.
    #[inline]
    pub fn claim_commit(&mut self, body: usize, stamp: u64) -> i64 {
        let mut claim = cavalry_buffer_claim_t {
            data: ptr::null_mut(),
            length: 0,
            position_after: 0,
            publication: ptr::null_mut(),
        };
        // SAFETY: a live handle used from this thread; on a claim, `data` is the body
        // of `length` bytes until the commit, which follows at once.
        unsafe {
            let position = cavalry_publication_try_claim(self.0, body, &mut claim);
            if position < 0 {
                return position;
            }
            if claim.length >= 8 {
                ptr::copy_nonoverlapping(stamp.to_le_bytes().as_ptr(), claim.data, 8);
            }
            cavalry_buffer_claim_commit(&mut claim)
        }
    }

    /// `cavalry_publication_position`.
    pub fn position(&self) -> Result<i64, String> {
        // SAFETY: a live handle used from this thread.
        let position = unsafe { cavalry_publication_position(self.0) };
        if position < 0 {
            return Err(last_error("cavalry_publication_position"));
        }
        Ok(position)
    }

    /// `cavalry_publication_close`.
    pub fn close(self) -> Result<(), String> {
        // SAFETY: closed once, here; the wrapper is consumed.
        let closed = unsafe { cavalry_publication_close(self.0) };
        std::mem::forget(self);
        if closed != 0 {
            return Err(last_error("cavalry_publication_close"));
        }
        Ok(())
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        // SAFETY: a wrapper dropped without `close` closes its handle once, here.
        unsafe { cavalry_publication_close(self.0) };
    }
}

/// The C fragment handler the rows poll with: hands the body to the Rust closure
/// `clientd` points at.
unsafe extern "C" fn trampoline(
    clientd: *mut c_void,
    buffer: *const u8,
    length: usize,
    _header: *const cavalry_header_t,
) {
    // SAFETY: `poll` passes a pointer to its `&mut dyn FnMut(&[u8])` for the call, and
    // the body is valid for the call.
    unsafe {
        let handler = &mut **clientd.cast::<&mut dyn FnMut(&[u8])>();
        handler(std::slice::from_raw_parts(buffer, length));
    }
}

impl Subscription {
    /// `cavalry_subscription_is_connected`.
    pub fn is_connected(&self) -> bool {
        // SAFETY: a live handle used from this thread.
        unsafe { cavalry_subscription_is_connected(self.0) }
    }

    /// `cavalry_subscription_poll` with the C handler above, which hands each body to
    /// `handler`: the fragments delivered, or the fault's message.
    #[inline]
    pub fn poll(
        &mut self,
        mut handler: &mut dyn FnMut(&[u8]),
        limit: usize,
    ) -> Result<usize, String> {
        let clientd: *mut &mut dyn FnMut(&[u8]) = &mut handler;
        // SAFETY: a live handle used from this thread, and the handler pointer is valid
        // for the call.
        let delivered =
            unsafe { cavalry_subscription_poll(self.0, Some(trampoline), clientd.cast(), limit) };
        if delivered < 0 {
            return Err(last_error("cavalry_subscription_poll"));
        }
        Ok(delivered as usize)
    }

    /// `cavalry_subscription_close`.
    pub fn close(self) -> Result<(), String> {
        // SAFETY: closed once, here; the wrapper is consumed.
        let closed = unsafe { cavalry_subscription_close(self.0) };
        std::mem::forget(self);
        if closed != 0 {
            return Err(last_error("cavalry_subscription_close"));
        }
        Ok(())
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // SAFETY: a wrapper dropped without `close` closes its handle once, here.
        unsafe { cavalry_subscription_close(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use cavalry_client::Offer;

    use super::*;
    use crate::codes::CAVALRY_ERR_TOO_LONG;
    use crate::error::offer_code;

    const ANCHOR: &str = "; see docs/reference/c-abi.md#result-codes";

    #[test]
    fn a_refusal_names_its_define_and_what_to_do_next() {
        for (code, name, next) in [
            (
                CAVALRY_PUBLICATION_NOT_CONNECTED,
                "CAVALRY_PUBLICATION_NOT_CONNECTED",
                "try again when one connects",
            ),
            (
                CAVALRY_PUBLICATION_BACK_PRESSURED,
                "CAVALRY_PUBLICATION_BACK_PRESSURED",
                "try again later",
            ),
            (
                CAVALRY_PUBLICATION_ADMIN_ACTION,
                "CAVALRY_PUBLICATION_ADMIN_ACTION",
                "try again at once",
            ),
            (
                CAVALRY_PUBLICATION_CLOSED,
                "CAVALRY_PUBLICATION_CLOSED",
                "connect a new client",
            ),
            (
                CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED,
                "CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED",
                "close the publication and add a new one",
            ),
            (
                -7,
                "a value spec/c-abi.md does not define",
                "report this message",
            ),
        ] {
            let message = refusal(code);
            assert!(
                message.starts_with(&format!("claim refused ({name}, {code}): ")),
                "{message}"
            );
            assert!(message.contains(next), "{message}");
            assert!(message.ends_with(ANCHOR), "{message}");
        }
    }

    #[test]
    fn an_error_refusal_carries_the_threads_error() {
        let too_long = Offer::TooLong {
            found: 9000,
            limit: 8160,
        };
        assert_eq!(offer_code(&too_long), CAVALRY_PUBLICATION_ERROR);
        let message = refusal(CAVALRY_PUBLICATION_ERROR);
        assert!(
            message.starts_with("claim refused (CAVALRY_PUBLICATION_ERROR, -6): "),
            "{message}"
        );
        assert!(
            message.contains(&format!("code {CAVALRY_ERR_TOO_LONG}")),
            "{message}"
        );
        assert!(message.contains(&too_long.to_string()), "{message}");
        assert!(message.ends_with(ANCHOR), "{message}");
    }
}
