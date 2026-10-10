//! The client API implementation, re-exported by the `cavalry` crate.
//!
//! Data plane: everything from `try_claim` on the publisher to the fragment handler on
//! the subscriber runs here or in the driver.
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
