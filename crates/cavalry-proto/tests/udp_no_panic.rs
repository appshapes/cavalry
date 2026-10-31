//! Every byte string either parses to a frame whose bytes round-trip, or is rejected
//! with one of the spec's reasons; every byte string either checks as a datagram whose
//! frames walk, or is rejected. Never a panic (invariant 2; CLAUDE.md gate 1, "frame
//! parsers are fuzzed"; ADR 0040). `just fuzz` runs the same property under cargo-fuzz
//! for longer; this runs on every `just test`.

use cavalry_proto::udp::{
    ControlFrame, Datagram, Heartbeat, Nak, RttMeasurement, SetupFrame, StatusMessage, parse,
};

/// Deterministic, so a failure reproduces: xorshift64.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

const REASONS: [&str; 10] = [
    "too-short",
    "version",
    "type",
    "length",
    "term-length",
    "mtu",
    "term-offset",
    "window",
    "nak-length",
    "datagram",
];

const TERM_LENGTHS: [u32; 3] = [65_536, 1 << 20, 1 << 30];

fn check(bytes: &[u8]) {
    for term_length in TERM_LENGTHS {
        match parse(bytes, term_length) {
            Ok(frame) => {
                let encoded: Vec<u8> = match frame {
                    ControlFrame::Setup(setup) => setup.to_bytes().to_vec(),
                    ControlFrame::StatusMessage(message) => {
                        message.to_bytes()[..message.frame_length()].to_vec()
                    }
                    ControlFrame::Nak(nak) => nak.to_bytes().to_vec(),
                    ControlFrame::RttMeasurement(rtt) => rtt.to_bytes().to_vec(),
                    ControlFrame::Heartbeat(heartbeat) => heartbeat.to_bytes().to_vec(),
                    ControlFrame::Data => Vec::new(),
                };
                assert!(
                    encoded.is_empty() || encoded[..] == bytes[..encoded.len()],
                    "{bytes:02x?}: to_bytes differs"
                );
            }
            Err(error) => assert!(REASONS.contains(&error.reason()), "{bytes:02x?}: {error}"),
        }
        match Datagram::check(bytes, term_length) {
            Ok(datagram) => {
                let frames = datagram.frames().count();
                assert_eq!(frames, datagram.frame_count(), "{bytes:02x?}: frame count");
                assert!(frames > 0, "{bytes:02x?}: a checked datagram has a frame");
                assert!(
                    datagram.end_term_offset() as u64 <= u64::from(term_length),
                    "{bytes:02x?}: past the term"
                );
            }
            Err(error) => assert!(REASONS.contains(&error.reason()), "{bytes:02x?}: {error}"),
        }
    }
}

fn valid_frames() -> Vec<Vec<u8>> {
    let setup = SetupFrame {
        flags: 0,
        term_offset: 1024,
        session_id: 7,
        stream_id: 1001,
        initial_term_id: -5,
        active_term_id: -3,
        term_length: 65_536,
        mtu_length: 1408,
        ttl: 0,
    };
    let status = StatusMessage {
        flags: 0,
        session_id: 7,
        stream_id: 1001,
        consumption_term_id: -5,
        consumption_term_offset: 4096,
        receiver_window_length: 131_072,
        receiver_id: 0x1122_3344_5566_7788,
        group_tag: None,
    };
    let tagged = StatusMessage {
        group_tag: Some(-1),
        ..status
    };
    let nak = Nak {
        flags: 0,
        session_id: 7,
        stream_id: 1001,
        term_id: -5,
        term_offset: 1024,
        length: 64,
    };
    let rtt = RttMeasurement {
        flags: 0x80,
        session_id: 7,
        stream_id: 1001,
        echo_timestamp: 1_700_000_000_000_000_000,
        reception_delta: 0,
        receiver_id: 1,
    };
    let heartbeat = Heartbeat {
        flags: 0xC0,
        term_offset: 2048,
        session_id: 7,
        stream_id: 1001,
        term_id: -2,
        reserved_value: 0,
    };
    let mut data = cavalry_proto::frame::DataFrameHeader::new(
        cavalry_proto::frame::FrameType::Data,
        cavalry_proto::frame::Flags::UNFRAGMENTED,
        40,
    )
    .expect("40 is a valid length")
    .with_term_offset(64)
    .with_session_id(7)
    .with_stream_id(1001)
    .with_term_id(-5)
    .to_bytes()
    .to_vec();
    data.extend_from_slice(b"cavalry!");
    data.resize(64, 0);
    vec![
        setup.to_bytes().to_vec(),
        status.to_bytes()[..36].to_vec(),
        tagged.to_bytes().to_vec(),
        nak.to_bytes().to_vec(),
        rtt.to_bytes().to_vec(),
        heartbeat.to_bytes().to_vec(),
        data,
    ]
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for _ in 0..20_000 {
        let length = (rng.next() % 100) as usize;
        let bytes: Vec<u8> = (0..length).map(|_| rng.next() as u8).collect();
        check(&bytes);
    }
}

#[test]
fn mutated_valid_frames_never_panic() {
    let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
    let frames = valid_frames();
    for frame in &frames {
        check(frame);
        for _ in 0..3_000 {
            let mut bytes = frame.clone();
            let flips = 1 + (rng.next() % 3) as usize;
            for _ in 0..flips {
                let at = (rng.next() as usize) % bytes.len();
                bytes[at] = rng.next() as u8;
            }
            check(&bytes);
            let cut = (rng.next() as usize) % (bytes.len() + 1);
            check(&bytes[..cut]);
        }
    }
}
