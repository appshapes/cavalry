//! The daemon: conductor, sender, receiver and idle strategies.
//!
//! Data plane. One driver per host owns the sockets; clients map its log buffers.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
