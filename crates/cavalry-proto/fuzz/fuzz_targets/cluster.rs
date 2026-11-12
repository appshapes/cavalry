//! cargo-fuzz target for the cluster protocol's parsers (ADR 0058): every input either
//! decodes on each stream or is rejected by name, as a members string either
//! parses or is refused by name, and as a file of a member's log's recordings is
//! either read, and then answers for any position, or refused by name (ADR 0103).

#![no_main]

use cavalry_proto::cluster::{
    EMPTY_MEMBER, LogRecordings, MAX_MEMBERS, Message, Stream, parse_members,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for stream in [Stream::Log, Stream::Consensus, Stream::Ingress, Stream::Egress, Stream::Snapshot] {
        let _ = Message::parse(stream, data);
    }
    let mut members = [EMPTY_MEMBER; MAX_MEMBERS];
    let _ = parse_members(data, &mut members, Some(0));
    if let Ok(file) = LogRecordings::read(data, 0) {
        for position in [i64::MIN, -1, 0, 96, i64::MAX] {
            let _ = file.recording_at(position);
        }
    }
});
