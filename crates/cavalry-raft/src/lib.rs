//! The replicated state machine on top of the durable log.
//!
//! Data plane: the append and commit path runs per message, so leader election and
//! membership live under the data-plane rules too, until a decision splits them out.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
