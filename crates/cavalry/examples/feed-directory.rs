//! The program of `conformance/upgrade/log-directory.sh`: one run of the feed that
//! Brigade's server keeps, on the directories and addresses its command line names,
//! doing what its standard input says. The script builds this file from two trees of
//! Cavalry, the one whose log directory it holds and this one, and has the first
//! write a log directory that the second opens in place
//! (`docs/explanation/what-brigades-server-relies-on.md`; ADR 0141).
//!
//! ```text
//! feed-directory --dir <driver directory> --log-dir <log directory>
//!                --control <host:port> --own <host:port>
//!                --shape pinned|moved-to [--roll-bytes <bytes>]
//! ```
//!
//! `--control` is the feed's control address and `--own` the address the feed reads
//! itself on in the `pinned` shape. `--roll-bytes` is how many bytes a recording
//! holds before a `release` rolls the feed; without it a release never rolls. The
//! commands and their answers are `crates/cavalry/tests/feed/process.rs`'s.
//!
//! The feed and its process are the test's own files, taken by path, so that the
//! directory is written by the same calls that `just test` holds. The script copies
//! those files beside this one into the older tree before it builds there: the files
//! use the calls of that tree and no later one.

use std::path::PathBuf;
use std::process::ExitCode;

#[path = "../tests/feed/mod.rs"]
mod feed;
use feed::FeedOptions;
use feed::process::{run, shape_named};

const USAGE: &str = "usage: feed-directory --dir <driver directory> --log-dir <log \
                     directory> --control <host:port> --own <host:port> --shape \
                     pinned|moved-to [--roll-bytes <bytes>]; see \
                     conformance/upgrade/log-directory.sh";

fn options() -> Result<FeedOptions, String> {
    let (mut dir, mut log_dir, mut control, mut own) = (None, None, None, None);
    let (mut shape, mut roll_bytes) = (None, u64::MAX);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} takes a value; {USAGE}"))?;
        match flag.as_str() {
            "--dir" => dir = Some(PathBuf::from(value)),
            "--log-dir" => log_dir = Some(PathBuf::from(value)),
            "--control" => control = Some(value),
            "--own" => own = Some(value),
            "--shape" => {
                shape = Some(shape_named(&value).ok_or_else(|| {
                    format!(
                        "--shape is `{value}`, and a feed's shape is pinned or moved-to; {USAGE}"
                    )
                })?);
            }
            "--roll-bytes" => {
                roll_bytes = value.parse().map_err(|_| {
                    format!("--roll-bytes is `{value}`, and it takes a count of bytes; {USAGE}")
                })?;
            }
            _ => return Err(format!("{flag} is no flag of this program; {USAGE}")),
        }
    }
    let missing = |flag: &str| format!("{flag} is missing; {USAGE}");
    Ok(FeedOptions {
        driver_dir: dir.ok_or_else(|| missing("--dir"))?,
        log_dir: log_dir.ok_or_else(|| missing("--log-dir"))?,
        control: control.ok_or_else(|| missing("--control"))?,
        own_endpoint: own.ok_or_else(|| missing("--own"))?,
        roll_bytes,
        shape: shape.ok_or_else(|| missing("--shape"))?,
    })
}

fn main() -> ExitCode {
    match options() {
        Ok(options) => match run(&options) {
            0 => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        },
        Err(error) => {
            eprintln!("feed-directory: {error}");
            ExitCode::from(2)
        }
    }
}
