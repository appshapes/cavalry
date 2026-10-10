//! The `cavalry` binary. Not built yet: `run`, `doctor`, `replay` and `console` (ADR
//! 0002) do not exist. Until they do, this prints one line and exits 1, so that no
//! check can pass against it by accident (CLAUDE.md, invariant 5).
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "cavalry {}: not built yet; see crates/cavalry/README.md",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::FAILURE
}
