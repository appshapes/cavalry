//! cargo-fuzz target for the UDP transport's parsers (ADR 0040): every input either
//! parses or is rejected by name, as a control frame and as a datagram.

#![no_main]

use cavalry_proto::udp::{Datagram, parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for term_length in [65_536u32, 1 << 20, 1 << 30] {
        let _ = parse(data, term_length);
        if let Ok(datagram) = Datagram::check(data, term_length) {
            let _ = datagram.frames().count();
        }
    }
});
