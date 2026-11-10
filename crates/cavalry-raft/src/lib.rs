#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

pub mod consensus;
pub mod memory;

pub use consensus::{
    Clock, Consensus, Counts, ElectionState, Io, Log, MembersRefused, NO_SESSION, Refused, Role,
};
