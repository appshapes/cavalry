//! Runs every vector in `spec/vectors/udp.json` against this crate: a frame vector
//! decodes to its fields and encodes back to its bytes or is rejected for the named
//! reason; a datagram vector walks to its frames or is rejected; a channel vector
//! parses to its fields or is refused with the named reason and code; an arithmetic
//! vector's fields are computed from its inputs (`spec/README.md`, "Conventions";
//! ADR 0040). `just conformance` runs this test alone; `just test` runs it with the rest.

use cavalry_proto::channel::{Channel, ChannelError};
use cavalry_proto::udp::{
    ControlFrame, Datagram, Heartbeat, Nak, RttMeasurement, SetupFrame, StatusMessage, image_start,
    parse, receiver_bounds, retransmit_window, sender_limit, status_message_threshold,
};
use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/udp.json");

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("vector bytes are hex"))
        .collect()
}

fn int(fields: &Value, name: &str) -> i64 {
    fields[name]
        .as_i64()
        .unwrap_or_else(|| panic!("vector lacks {name}"))
}

fn i32_of(fields: &Value, name: &str) -> i32 {
    i32::try_from(int(fields, name)).unwrap_or_else(|_| panic!("{name} fits i32"))
}

fn u32_of(fields: &Value, name: &str) -> u32 {
    u32::try_from(int(fields, name)).unwrap_or_else(|_| panic!("{name} fits u32"))
}

fn u8_of(fields: &Value, name: &str) -> u8 {
    u8::try_from(int(fields, name)).unwrap_or_else(|_| panic!("{name} fits u8"))
}

fn term_length(vector: &Value) -> u32 {
    u32_of(vector, "term_length")
}

fn frame_vector(name: &str, vector: &Value) {
    let bytes = hex(vector["bytes"].as_str().expect("bytes"));
    let result = parse(&bytes, term_length(vector));
    if let Some(reason) = vector.get("error") {
        let reason = reason.as_str().expect("error is a string");
        match result {
            Err(error) => assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}"),
            Ok(frame) => panic!("{name}: accepted, expected {reason}: {frame:?}"),
        }
        return;
    }
    let fields = &vector["fields"];
    let parsed = result.unwrap_or_else(|error| panic!("{name}: rejected: {error}"));
    let kind = vector["frame"]
        .as_str()
        .expect("a valid frame vector names its frame");
    let encoded: Vec<u8> = match (kind, parsed) {
        ("setup", ControlFrame::Setup(setup)) => {
            let expected = SetupFrame {
                flags: u8_of(fields, "flags"),
                term_offset: i32_of(fields, "term_offset"),
                session_id: i32_of(fields, "session_id"),
                stream_id: i32_of(fields, "stream_id"),
                initial_term_id: i32_of(fields, "initial_term_id"),
                active_term_id: i32_of(fields, "active_term_id"),
                term_length: i32_of(fields, "term_length"),
                mtu_length: i32_of(fields, "mtu_length"),
                ttl: i32_of(fields, "ttl"),
            };
            assert_eq!(setup, expected, "{name}: fields");
            setup.to_bytes().to_vec()
        }
        ("status-message", ControlFrame::StatusMessage(message)) => {
            let expected = StatusMessage {
                flags: u8_of(fields, "flags"),
                session_id: i32_of(fields, "session_id"),
                stream_id: i32_of(fields, "stream_id"),
                consumption_term_id: i32_of(fields, "consumption_term_id"),
                consumption_term_offset: i32_of(fields, "consumption_term_offset"),
                receiver_window_length: i32_of(fields, "receiver_window_length"),
                receiver_id: int(fields, "receiver_id"),
                group_tag: fields.get("group_tag").and_then(Value::as_i64),
            };
            assert_eq!(message, expected, "{name}: fields");
            assert_eq!(
                message.frame_length() as i64,
                int(fields, "frame_length"),
                "{name}: frame_length"
            );
            message.to_bytes()[..message.frame_length()].to_vec()
        }
        ("nak", ControlFrame::Nak(nak)) => {
            let expected = Nak {
                flags: u8_of(fields, "flags"),
                session_id: i32_of(fields, "session_id"),
                stream_id: i32_of(fields, "stream_id"),
                term_id: i32_of(fields, "term_id"),
                term_offset: i32_of(fields, "term_offset"),
                length: i32_of(fields, "length"),
            };
            assert_eq!(nak, expected, "{name}: fields");
            nak.to_bytes().to_vec()
        }
        ("rtt-measurement", ControlFrame::RttMeasurement(rtt)) => {
            let expected = RttMeasurement {
                flags: u8_of(fields, "flags"),
                session_id: i32_of(fields, "session_id"),
                stream_id: i32_of(fields, "stream_id"),
                echo_timestamp: int(fields, "echo_timestamp"),
                reception_delta: int(fields, "reception_delta"),
                receiver_id: int(fields, "receiver_id"),
            };
            assert_eq!(rtt, expected, "{name}: fields");
            rtt.to_bytes().to_vec()
        }
        ("heartbeat", ControlFrame::Heartbeat(heartbeat)) => {
            let expected = Heartbeat {
                flags: u8_of(fields, "flags"),
                term_offset: i32_of(fields, "term_offset"),
                session_id: i32_of(fields, "session_id"),
                stream_id: i32_of(fields, "stream_id"),
                term_id: i32_of(fields, "term_id"),
                reserved_value: int(fields, "reserved_value"),
            };
            assert_eq!(heartbeat, expected, "{name}: fields");
            heartbeat.to_bytes().to_vec()
        }
        (kind, other) => panic!("{name}: a {kind} vector parsed as {other:?}"),
    };
    assert_eq!(encoded, bytes, "{name}: to_bytes differs");
}

fn datagram_vector(name: &str, vector: &Value) {
    let bytes = hex(vector["bytes"].as_str().expect("bytes"));
    let result = Datagram::check(&bytes, term_length(vector));
    if let Some(reason) = vector.get("error") {
        let reason = reason.as_str().expect("error is a string");
        match result {
            Err(error) => assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}"),
            Ok(datagram) => panic!("{name}: accepted, expected {reason}: {datagram:?}"),
        }
        return;
    }
    let datagram = result.unwrap_or_else(|error| panic!("{name}: rejected: {error}"));
    let expected = vector["frames"].as_array().expect("frames");
    let frames: Vec<_> = datagram.frames().collect();
    assert_eq!(frames.len(), expected.len(), "{name}: frame count");
    assert_eq!(
        datagram.frame_count(),
        expected.len(),
        "{name}: frame_count"
    );
    for (frame, want) in frames.iter().zip(expected) {
        assert_eq!(
            i64::from(frame.header.term_offset()),
            int(want, "term_offset"),
            "{name}: term_offset"
        );
        assert_eq!(
            i64::from(frame.header.frame_length()),
            int(want, "frame_length"),
            "{name}: frame_length"
        );
    }
    let first = frames.first().expect("a valid datagram has a frame");
    assert_eq!(
        i64::from(datagram.first_term_offset()),
        i64::from(first.header.term_offset()),
        "{name}: first_term_offset"
    );
    assert_eq!(
        u64::from(datagram.end_term_offset()),
        u64::from(datagram.first_term_offset()) + bytes.len() as u64,
        "{name}: end_term_offset"
    );
}

fn channel_vector(name: &str, vector: &Value) {
    let string = vector["string"].as_str().expect("string");
    let result = Channel::parse(string.as_bytes());
    if let Some(reason) = vector.get("error") {
        let reason = reason.as_str().expect("error is a string");
        let code = int(vector, "code");
        match result {
            Err(error) => {
                assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}");
                assert_eq!(i64::from(error.code()), code, "{name}: code");
                if let ChannelError::Key { .. } = error {
                    assert!(
                        !error.key(string.as_bytes()).is_empty(),
                        "{name}: key slice"
                    );
                }
            }
            Ok(channel) => panic!("{name}: accepted, expected {reason}: {channel:?}"),
        }
        return;
    }
    let fields = &vector["fields"];
    let parsed = result.unwrap_or_else(|error| panic!("{name}: refused: {error}"));
    let Channel::Udp(udp) = parsed else {
        panic!("{name}: parsed as IPC");
    };
    assert_eq!(
        udp.endpoint().host(),
        fields["endpoint_host"].as_str().expect("endpoint_host"),
        "{name}: endpoint host"
    );
    assert_eq!(
        i64::from(udp.endpoint_port()),
        int(fields, "endpoint_port"),
        "{name}: endpoint port"
    );
    match fields["interface_host"].as_str() {
        None => assert!(udp.interface().is_none(), "{name}: interface"),
        Some(host) => {
            let interface = udp
                .interface()
                .unwrap_or_else(|| panic!("{name}: no interface"));
            assert_eq!(interface.host(), host, "{name}: interface host");
            assert_eq!(
                interface.port().map(i64::from),
                fields["interface_port"].as_i64(),
                "{name}: interface port"
            );
        }
    }
}

fn arithmetic_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let fields = &vector["fields"];
    match vector["rule"].as_str().expect("rule") {
        "sender-limit" => {
            let got = sender_limit(
                int(inputs, "sender_position"),
                int(inputs, "sender_limit"),
                i32_of(inputs, "consumption_term_id"),
                i32_of(inputs, "consumption_term_offset"),
                i32_of(inputs, "receiver_window_length"),
                i32_of(inputs, "initial_term_id"),
                u32_of(inputs, "term_length"),
            );
            assert_eq!(got.position, int(fields, "position"), "{name}: position");
            assert_eq!(
                got.accepted,
                fields["accepted"].as_bool().expect("accepted"),
                "{name}: accepted"
            );
            assert_eq!(
                got.sender_limit,
                int(fields, "sender_limit"),
                "{name}: sender_limit"
            );
        }
        "retransmit-window" => {
            let got = retransmit_window(
                int(inputs, "sender_position"),
                i32_of(inputs, "term_id"),
                i32_of(inputs, "term_offset"),
                i32_of(inputs, "initial_term_id"),
                u32_of(inputs, "term_length"),
            );
            assert_eq!(
                got.resend_position,
                int(fields, "resend_position"),
                "{name}: resend_position"
            );
            assert_eq!(got.bottom, int(fields, "bottom"), "{name}: bottom");
            assert_eq!(
                got.in_window,
                fields["in_window"].as_bool().expect("in_window"),
                "{name}: in_window"
            );
        }
        "receiver-bounds" => {
            let got = receiver_bounds(
                int(inputs, "last_sm_position"),
                int(inputs, "packet_position"),
                i32_of(inputs, "payload_length"),
                u32_of(inputs, "term_length"),
            );
            assert_eq!(
                got.under_run,
                fields["under_run"].as_bool().expect("under_run"),
                "{name}: under_run"
            );
            assert_eq!(
                got.over_run,
                fields["over_run"].as_bool().expect("over_run"),
                "{name}: over_run"
            );
            assert_eq!(
                got.accepted,
                fields["accepted"].as_bool().expect("accepted"),
                "{name}: accepted"
            );
        }
        "status-message-threshold" => {
            assert_eq!(
                i64::from(status_message_threshold(i32_of(
                    inputs,
                    "receiver_window_length"
                ))),
                int(fields, "threshold"),
                "{name}: threshold"
            );
        }
        "image-start" => {
            let got = image_start(
                i32_of(inputs, "initial_term_id"),
                i32_of(inputs, "active_term_id"),
                i32_of(inputs, "term_offset"),
                u32_of(inputs, "term_length"),
            );
            assert_eq!(got.position, int(fields, "position"), "{name}: position");
            assert_eq!(
                i64::from(got.receiver_window_length),
                int(fields, "receiver_window_length"),
                "{name}: receiver_window_length"
            );
        }
        rule => panic!("{name}: unknown rule {rule}"),
    }
}

#[test]
fn every_vector_in_spec_vectors_udp_json() {
    let doc: Value = serde_json::from_str(VECTORS).expect("udp.json parses");
    assert_eq!(doc["version"], 0, "the vectors are for version 0");
    let vectors = doc["vectors"].as_array().expect("vectors is an array");
    assert!(vectors.len() >= 90, "udp.py writes at least 90 vectors");
    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        match vector["kind"].as_str().expect("kind") {
            "frame" => frame_vector(name, vector),
            "datagram" => datagram_vector(name, vector),
            "channel" => channel_vector(name, vector),
            "arithmetic" => arithmetic_vector(name, vector),
            kind => panic!("{name}: unknown kind {kind}"),
        }
    }
}
