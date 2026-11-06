//! cargo-fuzz target for the cluster protocol's parsers (ADR 0058): every input either
//! decodes on each stream or is rejected by name, and as a members string either
//! parses or is refused by name.

#![no_main]

use cavalry_proto::cluster::{EMPTY_MEMBER, MAX_MEMBERS, Message, Stream, parse_members};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for stream in [Stream::Log, Stream::Consensus, Stream::Ingress, Stream::Egress, Stream::Snapshot] {
        let _ = Message::parse(stream, data);
    }
    let mut members = [EMPTY_MEMBER; MAX_MEMBERS];
    let _ = parse_members(data, &mut members, Some(0));
});
