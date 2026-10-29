#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

pub use cavalry_client::{
    BufferClaim, Cavalry, ClientError, Closed, DataFrameHeader, FOLLOW_LENGTH, Fault, Flags,
    FrameType, Image, MAX_IMAGES, NULL_LENGTH, NULL_POSITION, Offer, Publication, Recording,
    Replay, ReplayStopReason, Subscription,
};
pub use cavalry_driver::{Driver, DriverError, DriverThread, IdleStrategy, ThreadStart, spec};
pub use cavalry_log::index;

#[cfg(feature = "cli")]
pub use cavalry_control::{config, doctor};
