#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod client;
mod conductor;
mod error;
mod publication;
mod response;

pub use cavalry_shm::log::BufferClaim;
pub use client::{Cavalry, DIR_ENV};
pub use error::{ClientError, Closed};
pub use publication::{Offer, Publication};
