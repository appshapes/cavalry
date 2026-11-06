//! Every byte string, on every stream, either decodes to a message that encodes to the
//! same bytes and decodes again to the same message, or is rejected with one of the
//! spec's twelve reasons; and every members string either parses or is refused with
//! one of its four. Never a panic (invariant 2; CLAUDE.md gate 1, "frame parsers are
//! fuzzed"; ADR 0058). This runs on every `just test`.

use cavalry_proto::cluster::{EMPTY_MEMBER, MAX_MEMBERS, Message, Stream, parse_members};

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

const REASONS: [&str; 12] = [
    "too-short",
    "schema",
    "version",
    "template",
    "stream",
    "block-length",
    "length",
    "enum",
    "ascii",
    "member-id",
    "position",
    "term",
];
const MEMBERS_REASONS: [&str; 4] = [
    "members-grammar",
    "members-duplicate",
    "members-count",
    "members-self",
];
const STREAMS: [Stream; 5] = [
    Stream::Log,
    Stream::Consensus,
    Stream::Ingress,
    Stream::Egress,
    Stream::Snapshot,
];

fn check(bytes: &[u8]) {
    for stream in STREAMS {
        match Message::parse(stream, bytes) {
            Ok(message) => {
                let mut buffer = vec![0u8; bytes.len()];
                let length = message
                    .write(&mut buffer)
                    .expect("a decoded message encodes");
                assert_eq!(length, bytes.len(), "{bytes:02x?}");
                assert_eq!(
                    buffer, bytes,
                    "{bytes:02x?}: the message encodes to its bytes"
                );
                let again = Message::parse(stream, &buffer).expect("encoded bytes decode");
                assert_eq!(again, message, "{bytes:02x?}");
            }
            Err(error) => assert!(REASONS.contains(&error.reason()), "{bytes:02x?}: {error}"),
        }
    }
}

/// The valid vectors' bytes, the seeds every mutation starts from.
fn valid_messages() -> Vec<Vec<u8>> {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../../../spec/vectors/cluster.json")).expect("vectors");
    doc["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .filter(|v| v["kind"] == "message" && v.get("error").is_none())
        .map(|v| {
            let text = v["bytes"].as_str().expect("bytes");
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
                .collect()
        })
        .collect()
}

#[test]
fn no_byte_string_panics_the_message_parser() {
    let mut random = XorShift(0x2545_F491_4F6C_DD1D);
    let seeds = valid_messages();
    assert!(seeds.len() >= 30);
    for seed in &seeds {
        check(seed);
        for cut in 0..seed.len() {
            check(&seed[..cut]);
        }
        for _ in 0..400 {
            let mut bytes = seed.clone();
            for _ in 0..1 + random.next() % 3 {
                let at = (random.next() as usize) % bytes.len();
                bytes[at] = random.next() as u8;
            }
            check(&bytes);
            bytes.extend_from_slice(&random.next().to_le_bytes()[..(random.next() % 8) as usize]);
            check(&bytes);
        }
    }
    for _ in 0..20_000 {
        let length = (random.next() % 160) as usize;
        let bytes: Vec<u8> = (0..length).map(|_| random.next() as u8).collect();
        check(&bytes);
    }
}

#[test]
fn no_byte_string_panics_the_members_parser() {
    let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
    let alphabet = b"0123456789,|:.[]abh-";
    for _ in 0..20_000 {
        let mut members = [EMPTY_MEMBER; MAX_MEMBERS];
        let length = (random.next() % 120) as usize;
        let text: Vec<u8> = (0..length)
            .map(|_| alphabet[(random.next() as usize) % alphabet.len()])
            .collect();
        let self_id = if random.next().is_multiple_of(2) {
            None
        } else {
            Some((random.next() % 10) as i32)
        };
        match parse_members(&text, &mut members, self_id) {
            Ok(count) => assert!((1..=MAX_MEMBERS).contains(&count), "{text:?}"),
            Err(error) => assert!(
                MEMBERS_REASONS.contains(&error.reason()),
                "{text:?}: {error}"
            ),
        }
    }
    for _ in 0..2_000 {
        let bytes: Vec<u8> = (0..64).map(|_| random.next() as u8).collect();
        let mut members = [EMPTY_MEMBER; MAX_MEMBERS];
        let _ = parse_members(&bytes, &mut members, None);
    }
}
