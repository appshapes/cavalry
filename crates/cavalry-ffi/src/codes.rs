//! The header's defines: the ABI version, the Rust API's public constants, the six
//! result codes of a claim or an offer, the error codes `cavalry_errcode` returns, why
//! a client closed and why a replay ended (`spec/c-abi.md`, "Result codes", "Error
//! codes" and "Constants"). cbindgen writes each as a `#define`.

use std::ffi::c_int;

/// This page's version: the header's `CAVALRY_ABI_VERSION`, which
/// `cavalry_context_init` passes to `cavalry_context_init_v` and the library compares
/// with its own.
pub const CAVALRY_ABI_VERSION: c_int = 2;
/// The environment variable a context's default directory reads.
pub const CAVALRY_DIR_ENV: &str = "CAVALRY_DIR";
/// The images a subscription holds at most.
pub const CAVALRY_MAX_IMAGES: usize = 256;
/// The `cavalry:ipc` stream a committed log's replay is played into
/// (`spec/cluster.md`, "Members and streams").
pub const CAVALRY_SERVICE_LOG_STREAM_ID: i32 = 104;
// The literal is what cbindgen writes in the header; this holds it to the Rust API's.
const _: () = assert!(CAVALRY_SERVICE_LOG_STREAM_ID == cavalry_client::SERVICE_LOG_STREAM_ID);
/// No position: a replay from the recording's start, a fault without one.
pub const CAVALRY_NULL_POSITION: i64 = -1;
/// A replay to the recording's extent as it stands.
pub const CAVALRY_NULL_LENGTH: i64 = -1;
/// A replay that follows a live recording.
pub const CAVALRY_FOLLOW_LENGTH: i64 = i64::MAX;

/// A claim or an offer: no subscriber has a position on the stream and no recording is
/// live.
pub const CAVALRY_PUBLICATION_NOT_CONNECTED: i64 = -1;
/// A claim or an offer: the slowest subscriber is a publication window behind.
pub const CAVALRY_PUBLICATION_BACK_PRESSURED: i64 = -2;
/// A claim or an offer: the term rotated under the claim; try again at once.
pub const CAVALRY_PUBLICATION_ADMIN_ACTION: i64 = -3;
/// A claim or an offer: the client this publication belongs to is closed.
pub const CAVALRY_PUBLICATION_CLOSED: i64 = -4;
/// A claim or an offer: the stream reached the largest position its term length allows.
pub const CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED: i64 = -5;
/// A claim or an offer: the thread's error is set; `cavalry_errcode` says what.
pub const CAVALRY_PUBLICATION_ERROR: i64 = -6;

/// No failure yet on this thread.
pub const CAVALRY_ERR_NONE: c_int = 0;
/// The driver's refusal, `spec/client-protocol.md`'s code 1.
pub const CAVALRY_ERR_INVALID_CHANNEL: c_int = 1;
/// The driver's refusal, code 2.
pub const CAVALRY_ERR_UNKNOWN_SUBSCRIPTION: c_int = 2;
/// The driver's refusal, code 3.
pub const CAVALRY_ERR_UNKNOWN_PUBLICATION: c_int = 3;
/// The driver's refusal, code 4.
pub const CAVALRY_ERR_CHANNEL_ENDPOINT_ERROR: c_int = 4;
/// The driver's refusal, code 6.
pub const CAVALRY_ERR_UNKNOWN_COMMAND_TYPE: c_int = 6;
/// The driver's refusal, code 7.
pub const CAVALRY_ERR_MALFORMED_COMMAND: c_int = 7;
/// The driver's refusal, code 9.
pub const CAVALRY_ERR_UNKNOWN_HOST: c_int = 9;
/// The driver's refusal, code 10.
pub const CAVALRY_ERR_RESOURCE_TEMPORARILY_UNAVAILABLE: c_int = 10;
/// The driver's refusal, code 11.
pub const CAVALRY_ERR_GENERIC_ERROR: c_int = 11;
/// The driver's refusal, code 12.
pub const CAVALRY_ERR_STORAGE_SPACE: c_int = 12;
/// The driver's refusal, code 20.
pub const CAVALRY_ERR_NO_PUBLICATION_TO_RECORD: c_int = 20;
/// The driver's refusal, code 21.
pub const CAVALRY_ERR_UNKNOWN_RECORDING: c_int = 21;
/// The driver's refusal, code 22.
pub const CAVALRY_ERR_ALREADY_RECORDING: c_int = 22;
/// The driver's refusal, code 23.
pub const CAVALRY_ERR_POSITION_OUT_OF_RANGE: c_int = 23;
/// The driver's refusal, code 24.
pub const CAVALRY_ERR_RECORDING_BUSY: c_int = 24;
/// No driver at the directory.
pub const CAVALRY_ERR_NOT_FOUND: c_int = 100;
/// The driver at the directory closed, or its heartbeat is stale.
pub const CAVALRY_ERR_DRIVER_GONE: c_int = 101;
/// The client is closed; `cavalry_closed` says why.
pub const CAVALRY_ERR_CLOSED: c_int = 102;
/// The client holds as many subscriptions as the driver allows.
pub const CAVALRY_ERR_SUBSCRIPTIONS_FULL: c_int = 103;
/// The client follows as many replays as the driver allows at once.
pub const CAVALRY_ERR_REPLAYS_FULL: c_int = 104;
/// The client has as many replay starts waiting as its queue holds.
pub const CAVALRY_ERR_REPLAY_QUEUE_FULL: c_int = 105;
/// A command went unanswered for the response timeout, 10 s.
pub const CAVALRY_ERR_RESPONSE_TIMEOUT: c_int = 106;
/// The command ring refused a write, or failed a check.
pub const CAVALRY_ERR_COMMAND_RING: c_int = 107;
/// The broadcast buffer failed a check.
pub const CAVALRY_ERR_BROADCAST: c_int = 108;
/// The counters file failed a check.
pub const CAVALRY_ERR_COUNTERS: c_int = 109;
/// A command could not be encoded: a channel or a name too long.
pub const CAVALRY_ERR_PROTOCOL: c_int = 110;
/// A log buffer failed a check, or could not be opened.
pub const CAVALRY_ERR_LOG: c_int = 111;
/// The driver answered with something this version does not understand.
pub const CAVALRY_ERR_DRIVER: c_int = 112;
/// The operating system refused a step.
pub const CAVALRY_ERR_IO: c_int = 113;
/// A refusal whose code is outside 1 to 99; the raw code is in the message.
pub const CAVALRY_ERR_REFUSED: c_int = 114;
/// A poll stopped on an image that failed a check; `cavalry_subscription_fault` says
/// which.
pub const CAVALRY_ERR_FAULT_IMAGE: c_int = 120;
/// A poll stopped because the subscription holds `CAVALRY_MAX_IMAGES` images and the
/// driver announced another.
pub const CAVALRY_ERR_FAULT_IMAGES_FULL: c_int = 121;
/// A poll stopped because the client is closed.
pub const CAVALRY_ERR_FAULT_CLOSED: c_int = 122;
/// A message longer than the publication allows; the message names the length and
/// the limit.
pub const CAVALRY_ERR_TOO_LONG: c_int = 130;
/// A null handle or a null required pointer.
pub const CAVALRY_ERR_NULL: c_int = 131;
/// A claim is outstanding on the publication, so the call is refused until its commit
/// or abort.
pub const CAVALRY_ERR_CLAIM_OUTSTANDING: c_int = 132;
/// The claim struct names no claim outstanding on its publication.
pub const CAVALRY_ERR_NO_CLAIM: c_int = 133;
/// The call came from inside the subscription's own fragment handler.
pub const CAVALRY_ERR_POLLING: c_int = 134;
/// The header the program was built against is another version than the library's.
pub const CAVALRY_ERR_ABI_VERSION: c_int = 135;
/// A channel that is not UTF-8.
pub const CAVALRY_ERR_NOT_UTF8: c_int = 136;
/// An empty directory.
pub const CAVALRY_ERR_EMPTY_DIR: c_int = 137;
/// `cavalry_committed_log_entry_position` on a header that carries no position: one a
/// subscription's poll handed its handler.
pub const CAVALRY_ERR_NO_POSITION: c_int = 138;

/// Why a client closed: `cavalry_close` was called.
pub const CAVALRY_CLOSED_BY_USER: i32 = 1;
/// Why a client closed: the driver closed.
pub const CAVALRY_CLOSED_DRIVER_CLOSED: i32 = 2;
/// Why a client closed: the driver's heartbeat went stale.
pub const CAVALRY_CLOSED_DRIVER_TIMEOUT: i32 = 3;
/// Why a client closed: the driver timed this client out.
pub const CAVALRY_CLOSED_CLIENT_TIMEOUT: i32 = 4;
/// Why a client closed: the broadcast buffer lapped this client.
pub const CAVALRY_CLOSED_LAPPED: i32 = 5;
/// Why a client closed: a file or an event failed a check.
pub const CAVALRY_CLOSED_CORRUPT: i32 = 6;
/// Why a client closed: images were announced faster than a subscription polled.
pub const CAVALRY_CLOSED_OVERRUN: i32 = 7;

/// Why a replay ended: it reached the end of what it was asked to play.
pub const CAVALRY_REPLAY_STOP_ENDED: i32 = 0;
/// Why a replay ended: `cavalry_stop_replay` was called.
pub const CAVALRY_REPLAY_STOP_STOPPED: i32 = 1;
/// Why a replay ended: its publication was not connected.
pub const CAVALRY_REPLAY_STOP_NOT_CONNECTED: i32 = 2;
/// Why a replay ended: the driver failed to play it.
pub const CAVALRY_REPLAY_STOP_FAILED: i32 = 3;

/// The header's BEGIN flag: the first fragment of a message.
pub const CAVALRY_FLAGS_BEGIN: u8 = 0x80;
/// The header's END flag: the last fragment of a message.
pub const CAVALRY_FLAGS_END: u8 = 0x40;
/// The header's EOS flag: the end of the stream.
pub const CAVALRY_FLAGS_EOS: u8 = 0x20;
/// BEGIN and END together: a message in one frame.
pub const CAVALRY_FLAGS_UNFRAGMENTED: u8 = 0xC0;
/// The header's type for a padding frame.
pub const CAVALRY_FRAME_TYPE_PAD: u16 = 0;
/// The header's type for a data frame.
pub const CAVALRY_FRAME_TYPE_DATA: u16 = 1;
