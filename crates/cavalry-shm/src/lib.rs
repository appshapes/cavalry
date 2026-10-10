//! Memory-mapped log buffers, ring buffers and the counters file.
//!
//! Data plane, and one of the two crates where `unsafe` is permitted. Every `unsafe`
//! block carries a `// SAFETY:` comment, and new `unsafe` needs a `loom` test in the
//! same commit.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
