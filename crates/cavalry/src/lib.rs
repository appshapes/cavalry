//! The one crate users touch: the client API and an embeddable driver as a library, and
//! the `cavalry` binary (`run`, `doctor`, `replay`, `console`).
//!
//! This crate never grows logic of its own beyond re-exports and the binary's `main`
//! (ADR 0002); `just lint` checks that this file holds only re-exports.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
