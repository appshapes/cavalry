#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod client;
mod codes;
mod committed_log;
mod context;
mod error;
mod handle;
pub mod harness;
mod publication;
mod recording;
mod replay;
mod strings;
mod subscription;
mod version;

pub use client::*;
pub use codes::*;
pub use committed_log::*;
pub use context::*;
pub use error::{cavalry_errcode, cavalry_errmsg};
pub use publication::*;
pub use recording::*;
pub use replay::*;
pub use subscription::*;
pub use version::*;

// Every handle may move between threads between calls (spec/c-abi.md, "Threads"),
// which holds only while each handle type is `Send`; a `cavalry-client` change that
// made one `!Send` would otherwise pass unnoticed until a client used it so. The
// subscription handle is `Send` by an `unsafe impl` (subscription.rs), which the check
// on the handle cannot fail, so the client type it wraps is checked on its own: the
// impl's SAFETY comment rests on it.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<cavalry_context_t>();
    assert_send::<cavalry_t>();
    assert_send::<cavalry_publication_t>();
    assert_send::<cavalry_subscription_t>();
    assert_send::<cavalry_client::Subscription>();
    assert_send::<cavalry_recording_t>();
    assert_send::<cavalry_replay_t>();
    assert_send::<cavalry_committed_log_t>();
};
