//! cargo-fuzz target for the channel URI parser (ADR 0040): every input either parses
//! or is refused by name.

#![no_main]

use cavalry_proto::channel::Channel;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Err(error) = Channel::parse(data) {
        let _ = error.key(data);
    }
});
