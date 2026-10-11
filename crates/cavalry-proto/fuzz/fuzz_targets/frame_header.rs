//! The property tests/no_panic.rs checks on every commit, under libFuzzer for longer:
//! any bytes either parse to a header whose bytes round-trip, or are rejected with one
//! of the spec's four reasons, and nothing panics.
#![no_main]

use cavalry_proto::frame::{DataFrameHeader, HEADER_LENGTH};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    match DataFrameHeader::parse(data) {
        Ok(header) => {
            let frame_length = usize::try_from(header.frame_length()).expect("validated");
            assert!(frame_length >= HEADER_LENGTH && frame_length <= data.len());
            assert_eq!(&header.to_bytes()[..], &data[..HEADER_LENGTH]);
            assert!(header.aligned_length() >= frame_length);
        }
        Err(error) => assert!(["too-short", "version", "type", "length"].contains(&error.reason())),
    }
});
