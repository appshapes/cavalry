#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod client;
mod codes;
mod context;
mod error;
mod handle;
mod publication;
mod recording;
mod replay;
mod strings;
mod subscription;
mod version;

pub use client::*;
pub use codes::*;
pub use context::*;
pub use error::{cavalry_errcode, cavalry_errmsg};
pub use publication::*;
pub use recording::*;
pub use replay::*;
pub use subscription::*;
pub use version::*;

// Every handle may move between threads between calls (spec/c-abi.md, "Threads"),
// which holds only while each handle type is `Send`; a `cavalry-client` change that
// made one `!Send` would otherwise pass unnoticed until a client used it so.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<cavalry_context_t>();
    assert_send::<cavalry_t>();
    assert_send::<cavalry_publication_t>();
    assert_send::<cavalry_subscription_t>();
    assert_send::<cavalry_recording_t>();
    assert_send::<cavalry_replay_t>();
};
