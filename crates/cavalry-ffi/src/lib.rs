//! The `extern "C"` ABI and its cbindgen header.
//!
//! Data plane, and the other crate where `unsafe` is permitted: every non-Rust client's
//! messages pass through here. Buffer lifetimes across the ABI are by convention, so
//! debug builds poison released buffers.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
