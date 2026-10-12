#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

pub mod broadcast;
pub mod command_ring;
pub mod counters;
pub mod log;
pub(crate) mod sync;
// What the two ring layouts share: the header, the record word, the trailer slots.
pub(crate) mod ring;
