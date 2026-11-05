//! A subscription's handle, its poll with the C fragment handler, its images and the
//! header values a handler reads (`spec/c-abi.md`, "Structs", "Functions",
//! subscription, image and header, "Calls during a claim or a poll", "A frame's
//! position").

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;

use cavalry_client::{Image, Subscription};
use cavalry_proto::frame::DataFrameHeader;

use crate::codes::*;
use crate::error::{self, DOC};
use crate::handle;
use crate::strings;

/// Whether a poll runs: the state word every function of the handle reads first.
// not a state machine: the word records whether the caller's poll is running, a borrow the page replaces by convention, and no counter counts a caller's polls
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Polling,
}

/// The subscription's constants, copied out of it when the handle is made, so that
/// `cavalry_subscription_constants` answers from inside the handler without reaching
/// the subscription the running poll borrows.
#[derive(Clone, Copy)]
struct Constants {
    registration_id: i64,
    stream_id: i32,
}

/// A subscription; made by `cavalry_add_subscription`, freed by
/// `cavalry_subscription_close`.
pub struct cavalry_subscription_t {
    state: State,
    fault: cavalry_fault_t,
    channel: CString,
    constants: Constants,
    /// The views `cavalry_subscription_image_at_index` hands out, one per image,
    /// rebuilt when the images change and valid until the next poll or close.
    views: Vec<cavalry_image_t>,
    subscription: Subscription,
}

/// One of a subscription's images, as `cavalry_subscription_image_at_index` gives it:
/// valid until the subscription's next poll or close.
pub struct cavalry_image_t {
    subscription: *const cavalry_subscription_t,
    correlation_id: i64,
    session_id: i32,
    initial_term_id: i32,
    position_bits_to_shift: usize,
    source_identity: CString,
}

/// The frame header a poll hands the fragment handler, valid for the call.
pub struct cavalry_header_t {
    header: DataFrameHeader,
}

/// The data frame header's fields, in the order and the types of `spec/frame.md`.
#[repr(C)]
pub struct cavalry_header_values_frame_t {
    /// The frame's length, header included.
    pub frame_length: i32,
    /// The header's version, 0.
    pub version: i8,
    /// BEGIN, END and EOS, `CAVALRY_FLAGS_*`.
    pub flags: u8,
    /// The frame's type, `CAVALRY_FRAME_TYPE_*`.
    pub r#type: u16,
    /// The frame's offset in its term.
    pub term_offset: i32,
    /// The publication's session id.
    pub session_id: i32,
    /// The stream id.
    pub stream_id: i32,
    /// The term the frame is in.
    pub term_id: i32,
    /// The reserved value the publisher set, 0 unless set.
    pub reserved_value: i64,
}

/// What `cavalry_header_values` fills: the frame's fields, nested as Aeron's.
#[repr(C)]
pub struct cavalry_header_values_t {
    /// The frame's fields.
    pub frame: cavalry_header_values_frame_t,
}

/// What the subscription's last poll stopped on; 0 when it returned a count.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct cavalry_fault_t {
    /// `CAVALRY_ERR_FAULT_IMAGE`, `_IMAGES_FULL`, `_CLOSED`, or 0.
    pub code: i32,
    /// The image's session id for 120 and 121, else 0.
    pub session_id: i32,
    /// The reader's position when the check failed, for 120, else
    /// `CAVALRY_NULL_POSITION`.
    pub position: i64,
}

/// A subscription's constants, the strings valid for the handle's life.
#[repr(C)]
pub struct cavalry_subscription_constants_t {
    /// The channel.
    pub channel: *const c_char,
    /// The id under which the driver knows the subscription.
    pub registration_id: i64,
    /// The stream id.
    pub stream_id: i32,
}

/// An image's constants, the strings valid for the handle's life.
#[repr(C)]
pub struct cavalry_image_constants_t {
    /// Where the stream came from: `cavalry:ipc`, or the sender's address over UDP.
    pub source_identity: *const c_char,
    /// The id under which the driver announced the image.
    pub correlation_id: i64,
    /// The base-2 logarithm of the term length: a frame's position is its term count
    /// shifted left by this, plus its term offset.
    pub position_bits_to_shift: usize,
    /// The publication's session id.
    pub session_id: i32,
    /// The id of the log's first term.
    pub initial_term_id: i32,
}

/// The C fragment handler: the body as a view into the log and the frame's header,
/// both valid for the call.
pub type cavalry_fragment_handler_t = Option<
    unsafe extern "C" fn(
        clientd: *mut c_void,
        buffer: *const u8,
        length: usize,
        header: *const cavalry_header_t,
    ),
>;

const NO_FAULT: cavalry_fault_t = cavalry_fault_t {
    code: 0,
    session_id: 0,
    position: CAVALRY_NULL_POSITION,
};

pub(crate) fn subscription_handle(subscription: Subscription) -> *mut cavalry_subscription_t {
    handle::give(cavalry_subscription_t {
        state: State::Idle,
        fault: NO_FAULT,
        channel: strings::c_string(subscription.channel()),
        constants: Constants {
            registration_id: subscription.registration_id(),
            stream_id: subscription.stream_id(),
        },
        views: Vec::new(),
        subscription,
    })
}

/// Reads the state word through the handle's pointer, without a reference to the
/// wrapped subscription.
///
/// # Safety
///
/// `subscription` is a live handle.
unsafe fn state(subscription: *const cavalry_subscription_t) -> State {
    // SAFETY: a live handle; the read touches the state field alone, which a running
    // poll does not borrow.
    unsafe { ptr::addr_of!((*subscription).state).read() }
}

fn polling(function: &str) {
    error::set(
        CAVALRY_ERR_POLLING,
        format_args!(
            "{function} refused (polling): the call came from inside this subscription's own \
             fragment handler, while its poll runs; make it after the poll returns; see \
             {DOC}#calls-during-a-claim-or-a-poll"
        ),
    );
}

/// Polls the subscription's images in turn, handing each fragment to `handler` with
/// `clientd`, up to `fragment_limit` fragments; returns the fragments delivered, or
/// −1 with the fault as the error and in `cavalry_subscription_fault`.
///
/// # Safety
///
/// `subscription` is null or a live handle not in use by another call; `handler` is
/// null or a function that returns, without unwinding or a `longjmp`, and holds the
/// rules of "Calls during a claim or a poll".
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_subscription_poll(
    subscription: *mut cavalry_subscription_t,
    handler: cavalry_fragment_handler_t,
    clientd: *mut c_void,
    fragment_limit: usize,
) -> c_int {
    if subscription.is_null() {
        error::null("cavalry_subscription_poll", "subscription");
        return -1;
    }
    let Some(handler) = handler else {
        error::null("cavalry_subscription_poll", "handler");
        return -1;
    };
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(subscription) } == State::Polling {
        polling("cavalry_subscription_poll");
        return -1;
    }
    let limit = fragment_limit.min(c_int::MAX as usize);
    // SAFETY: the state is Idle, so no poll borrows the subscription, and the handle is
    // used from one thread at a time: this is the one reference to it. The state word
    // is written through its own field so that a re-entrant call from the handler
    // reads it without touching the borrowed subscription.
    let result = unsafe {
        ptr::addr_of_mut!((*subscription).state).write(State::Polling);
        let inner = &mut *ptr::addr_of_mut!((*subscription).subscription);
        let result = inner.poll(
            |body, header| {
                let view = cavalry_header_t { header: *header };
                // The caller's handler, by the convention above, inside the enclosing
                // unsafe block; the body and the header are valid for the call.
                handler(clientd, body.as_ptr(), body.len(), &view);
            },
            limit,
        );
        ptr::addr_of_mut!((*subscription).state).write(State::Idle);
        result
    };
    match result {
        Ok(delivered) => {
            // SAFETY: the fault field alone.
            unsafe { ptr::addr_of_mut!((*subscription).fault).write(NO_FAULT) };
            delivered as c_int
        }
        Err(fault) => {
            let code = error::set_fault(&fault);
            let (session_id, position) = match fault {
                cavalry_client::Fault::Image {
                    session_id,
                    position,
                    ..
                } => (session_id, position),
                cavalry_client::Fault::ImagesFull { session_id } => {
                    (session_id, CAVALRY_NULL_POSITION)
                }
                cavalry_client::Fault::Closed => (0, CAVALRY_NULL_POSITION),
            };
            // SAFETY: the fault field alone.
            unsafe {
                ptr::addr_of_mut!((*subscription).fault).write(cavalry_fault_t {
                    code,
                    session_id,
                    position,
                })
            };
            -1
        }
    }
}

/// Fills `fault` with what the last poll stopped on.
///
/// # Safety
///
/// `subscription` is null or a live handle; `fault` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_subscription_fault(
    subscription: *const cavalry_subscription_t,
    fault: *mut cavalry_fault_t,
) -> c_int {
    if subscription.is_null() {
        error::null("cavalry_subscription_fault", "subscription");
        return -1;
    }
    if fault.is_null() {
        error::null("cavalry_subscription_fault", "fault");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(subscription) } == State::Polling {
        polling("cavalry_subscription_fault");
        return -1;
    }
    // SAFETY: the fault field alone, and a writable struct.
    unsafe { *fault = ptr::addr_of!((*subscription).fault).read() };
    0
}

/// Reads through the subscription when no poll runs, or sets the error.
///
/// # Safety
///
/// `subscription` is a live handle not in use by another call.
unsafe fn read<T>(
    function: &str,
    subscription: *const cavalry_subscription_t,
    refused: T,
    run: impl FnOnce(&Subscription) -> T,
) -> T {
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(subscription) } == State::Polling {
        polling(function);
        return refused;
    }
    // SAFETY: no poll borrows the subscription.
    run(unsafe { &*ptr::addr_of!((*subscription).subscription) })
}

/// How many images the subscription holds.
///
/// # Safety
///
/// `subscription` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_subscription_image_count(
    subscription: *const cavalry_subscription_t,
) -> c_int {
    if subscription.is_null() {
        error::null("cavalry_subscription_image_count", "subscription");
        return -1;
    }
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_subscription_image_count",
            subscription,
            -1,
            |inner| inner.image_count() as c_int,
        )
    }
}

/// The image at `index`, valid until the subscription's next poll or close; null past
/// the count, with no error set.
///
/// # Safety
///
/// `subscription` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_subscription_image_at_index(
    subscription: *mut cavalry_subscription_t,
    index: usize,
) -> *mut cavalry_image_t {
    if subscription.is_null() {
        error::null("cavalry_subscription_image_at_index", "subscription");
        return ptr::null_mut();
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(subscription) } == State::Polling {
        polling("cavalry_subscription_image_at_index");
        return ptr::null_mut();
    }
    // SAFETY: no poll runs, and the handle is used from one thread at a time, so this
    // is the one reference to it; the views are rebuilt here, on the control plane,
    // and the pointer given out is into the handle's own vector, which the next poll
    // or close may replace ("Lifetimes").
    unsafe {
        let handle = &mut *subscription;
        let images = handle.subscription.images();
        let same = handle.views.len() == images.len()
            && handle
                .views
                .iter()
                .zip(images)
                .all(|(view, image)| view.correlation_id == image.correlation_id());
        if !same {
            handle.views = images
                .iter()
                .map(|image| view_of(subscription, image))
                .collect();
        }
        match handle.views.get_mut(index) {
            Some(view) => view as *mut cavalry_image_t,
            None => ptr::null_mut(),
        }
    }
}

fn view_of(subscription: *const cavalry_subscription_t, image: &Image) -> cavalry_image_t {
    let positions = image.positions();
    cavalry_image_t {
        subscription,
        correlation_id: image.correlation_id(),
        session_id: image.session_id(),
        initial_term_id: positions.initial_term_id(),
        position_bits_to_shift: positions.term_length().trailing_zeros() as usize,
        source_identity: strings::c_string(image.source_identity()),
    }
}

/// Finds the live image a view names, or sets the error.
///
/// # Safety
///
/// `image` is a view a live subscription handle gave, valid until that
/// subscription's next poll or close.
unsafe fn live<'a>(function: &str, image: *const cavalry_image_t) -> Option<&'a Image> {
    // SAFETY: the view and its subscription are live, by the caller's convention, and
    // no poll runs on it (the view would be invalid inside one).
    unsafe {
        let view = &*image;
        let subscription = &*ptr::addr_of!((*view.subscription).subscription);
        let found = subscription
            .images()
            .iter()
            .find(|candidate| candidate.correlation_id() == view.correlation_id);
        if found.is_none() {
            error::set(
                CAVALRY_ERR_NULL,
                format_args!(
                    "{function} refused (null): the image is gone from its subscription; take \
                     the image again after the poll; see {DOC}#lifetimes"
                ),
            );
        }
        found
    }
}

/// Fills `constants` from the image.
///
/// # Safety
///
/// `image` is null or a live view; `constants` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_image_constants(
    image: *const cavalry_image_t,
    constants: *mut cavalry_image_constants_t,
) -> c_int {
    if image.is_null() {
        error::null("cavalry_image_constants", "image");
        return -1;
    }
    if constants.is_null() {
        error::null("cavalry_image_constants", "constants");
        return -1;
    }
    // SAFETY: a live view and a writable struct, by the caller's conventions.
    unsafe {
        let view = &*image;
        *constants = cavalry_image_constants_t {
            source_identity: view.source_identity.as_ptr(),
            correlation_id: view.correlation_id,
            position_bits_to_shift: view.position_bits_to_shift,
            session_id: view.session_id,
            initial_term_id: view.initial_term_id,
        };
    }
    0
}

/// The position the subscription has read the image to.
///
/// # Safety
///
/// `image` is null or a live view.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_image_position(image: *const cavalry_image_t) -> i64 {
    if image.is_null() {
        error::null("cavalry_image_position", "image");
        return -1;
    }
    // SAFETY: the caller's convention above.
    match unsafe { live("cavalry_image_position", image) } {
        Some(found) => found.position(),
        None => -1,
    }
}

/// Whether the image's stream has ended at the position read.
///
/// # Safety
///
/// `image` is null or a live view.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_image_is_end_of_stream(image: *const cavalry_image_t) -> bool {
    if image.is_null() {
        error::null("cavalry_image_is_end_of_stream", "image");
        return false;
    }
    // SAFETY: the caller's convention above.
    match unsafe { live("cavalry_image_is_end_of_stream", image) } {
        Some(found) => found.is_end_of_stream(),
        None => false,
    }
}

/// Fills `values` with the header's fields.
///
/// # Safety
///
/// `header` is null or the header a poll handed the handler, during that call;
/// `values` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_header_values(
    header: *const cavalry_header_t,
    values: *mut cavalry_header_values_t,
) -> c_int {
    if header.is_null() {
        error::null("cavalry_header_values", "header");
        return -1;
    }
    if values.is_null() {
        error::null("cavalry_header_values", "values");
        return -1;
    }
    // SAFETY: the handler's header, valid for the call, and a writable struct.
    unsafe {
        let frame = &(*header).header;
        *values = cavalry_header_values_t {
            frame: cavalry_header_values_frame_t {
                frame_length: frame.frame_length(),
                version: frame.version(),
                flags: frame.flags().bits(),
                r#type: frame.frame_type() as u16,
                term_offset: frame.term_offset(),
                session_id: frame.session_id(),
                stream_id: frame.stream_id(),
                term_id: frame.term_id(),
                reserved_value: frame.reserved_value(),
            },
        };
    }
    0
}

/// Whether the subscription has an image.
///
/// # Safety
///
/// `subscription` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_subscription_is_connected(
    subscription: *const cavalry_subscription_t,
) -> bool {
    if subscription.is_null() {
        error::null("cavalry_subscription_is_connected", "subscription");
        return false;
    }
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_subscription_is_connected",
            subscription,
            false,
            Subscription::is_connected,
        )
    }
}

/// Whether the subscription's client is closed.
///
/// # Safety
///
/// As for `cavalry_subscription_is_connected`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_subscription_is_closed(
    subscription: *const cavalry_subscription_t,
) -> bool {
    if subscription.is_null() {
        error::null("cavalry_subscription_is_closed", "subscription");
        return false;
    }
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_subscription_is_closed",
            subscription,
            false,
            Subscription::is_closed,
        )
    }
}

/// Fills `constants` from the copy the handle took when it was made; allowed from
/// inside the handler.
///
/// # Safety
///
/// `subscription` is null or a live handle; `constants` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_subscription_constants(
    subscription: *const cavalry_subscription_t,
    constants: *mut cavalry_subscription_constants_t,
) -> c_int {
    if subscription.is_null() {
        error::null("cavalry_subscription_constants", "subscription");
        return -1;
    }
    if constants.is_null() {
        error::null("cavalry_subscription_constants", "constants");
        return -1;
    }
    // SAFETY: a live handle and a writable struct, by the caller's conventions. The
    // reads go through the handle's pointer to its `channel` and `constants` fields
    // alone, the copies `subscription_handle` took, and never form a reference to the
    // wrapped subscription, which a running poll holds as `&mut` while it calls the
    // handler this call may come from.
    unsafe {
        let channel = (*ptr::addr_of!((*subscription).channel)).as_ptr();
        let copy = ptr::addr_of!((*subscription).constants).read();
        *constants = cavalry_subscription_constants_t {
            channel,
            registration_id: copy.registration_id,
            stream_id: copy.stream_id,
        };
    }
    0
}

/// Closes the subscription and frees the handle; refused from inside its own handler.
///
/// # Safety
///
/// `subscription` is null or a live handle, closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_subscription_close(
    subscription: *mut cavalry_subscription_t,
) -> c_int {
    if subscription.is_null() {
        error::null("cavalry_subscription_close", "subscription");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(subscription) } == State::Polling {
        polling("cavalry_subscription_close");
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention.
    let handle = unsafe { handle::take_out(subscription) };
    match handle.subscription.close() {
        Ok(()) => 0,
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}
