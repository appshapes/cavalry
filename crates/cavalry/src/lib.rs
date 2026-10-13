#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

pub use cavalry_driver::{Driver, DriverError, IdleStrategy, spec};

#[cfg(feature = "cli")]
pub use cavalry_control::{config, doctor};
