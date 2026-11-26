#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

pub mod broadcast;
pub mod clock;
pub mod command_ring;
pub mod counters;
pub mod log;
pub mod spsc;
// A new shared file is given its room before it is mapped (ADR 0175).
pub(crate) mod allocate;
// Every mapping is made resident when it is made (ADR 0027).
pub(crate) mod pretouch;
pub(crate) mod sync;
// What the two ring layouts share: the header, the record word, the trailer slots.
pub(crate) mod ring;
