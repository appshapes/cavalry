#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod client;
mod conductor;
mod error;
mod publication;
mod recording;
mod replay;
mod response;
mod subscription;

pub use cavalry_proto::client_protocol::{
    FOLLOW_LENGTH, NULL_LENGTH, NULL_POSITION, ReplayStopReason,
};
pub use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};
pub use cavalry_shm::log::BufferClaim;
pub use client::{Cavalry, DIR_ENV};
pub use error::{ClientError, Closed};
pub use publication::{Offer, Publication};
pub use recording::Recording;
pub use replay::Replay;
pub use subscription::{Fault, Image, MAX_IMAGES, Subscription};
