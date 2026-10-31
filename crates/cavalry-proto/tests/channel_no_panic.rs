//! Every byte string either parses to a channel or is refused with one of the spec's
//! four reasons, every refusal error code 1. Never a panic (invariant 2; ADR 0040).
//! `just fuzz` runs the same property under cargo-fuzz for longer; this runs on every
//! `just test`.

use cavalry_proto::channel::{Channel, ChannelError};

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

const REASONS: [&str; 4] = ["grammar", "key", "endpoint", "multicast"];

fn check(bytes: &[u8]) {
    match Channel::parse(bytes) {
        Ok(Channel::Ipc) => assert_eq!(bytes, b"cavalry:ipc", "{bytes:02x?}: ipc"),
        Ok(Channel::Udp(udp)) => {
            assert!(udp.endpoint_port() >= 1, "{bytes:02x?}: endpoint port");
            assert!(
                !udp.endpoint().host().is_empty(),
                "{bytes:02x?}: endpoint host"
            );
            assert!(
                !udp.endpoint().is_multicast(),
                "{bytes:02x?}: multicast accepted"
            );
            if let Some(interface) = udp.interface() {
                assert!(!interface.host().is_empty(), "{bytes:02x?}: interface host");
                assert_ne!(interface.port(), Some(0), "{bytes:02x?}: interface port 0");
            }
        }
        Err(error) => {
            assert!(REASONS.contains(&error.reason()), "{bytes:02x?}: {error}");
            assert_eq!(error.code(), 1, "{bytes:02x?}: code");
            if let ChannelError::Key { at, len } = error {
                assert!(at + len <= bytes.len(), "{bytes:02x?}: key slice");
            }
            let _ = error.to_string();
        }
    }
}

const VALID: [&str; 6] = [
    "cavalry:ipc",
    "cavalry:udp?endpoint=10.0.0.2:40123",
    "cavalry:udp?endpoint=grip-link:40123|interface=10.0.0.1",
    "cavalry:udp?endpoint=[fd00::2]:40123|interface=[fd00::1]:5000",
    "cavalry:udp?interface=10.0.0.1:1|endpoint=bench-link:65535",
    "cavalry:udp?endpoint=a.b-c.d:1",
];

const ALPHABET: &[u8] = b"cavlry:iudp?endoint=.-|[]0123456789abcdef:gx \xff";

#[test]
fn random_strings_never_panic() {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for _ in 0..20_000 {
        let length = (rng.next() % 64) as usize;
        let bytes: Vec<u8> = (0..length)
            .map(|_| ALPHABET[(rng.next() as usize) % ALPHABET.len()])
            .collect();
        check(&bytes);
    }
}

#[test]
fn mutated_valid_channels_never_panic() {
    let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
    for valid in VALID {
        check(valid.as_bytes());
        for _ in 0..3_000 {
            let mut bytes = valid.as_bytes().to_vec();
            let flips = 1 + (rng.next() % 3) as usize;
            for _ in 0..flips {
                let at = (rng.next() as usize) % bytes.len();
                bytes[at] = ALPHABET[(rng.next() as usize) % ALPHABET.len()];
            }
            check(&bytes);
            let cut = (rng.next() as usize) % (bytes.len() + 1);
            check(&bytes[..cut]);
        }
    }
}
