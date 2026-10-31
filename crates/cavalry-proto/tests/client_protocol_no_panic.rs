//! Every byte string, under every type id, either decodes to a message that encodes to
//! the same length and decodes again to the same message, or is rejected with one of
//! the spec's four reasons. Never a panic (invariant 2; CLAUDE.md gate 1, "frame
//! parsers are fuzzed"; ADR 0019). The bytes themselves need not round-trip: a string's
//! padding is zero when written and not read, so a padding byte a mutation set is
//! decoded past and written back as zero. This runs on every `just test`.

use cavalry_proto::client_protocol::{Command, CommandType, Event, EventType};

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

const REASONS: [&str; 4] = ["too-short", "type", "string", "length"];

const COMMAND_TYPES: [i32; 14] = [
    1, 2, 3, 4, 5, 6, 11, 0x0101, 0x0102, 0x0103, 0x0104, 0x0105, 0x0106, 0x0107,
];
const EVENT_TYPES: [i32; 12] = [
    0x0F01, 0x0F02, 0x0F03, 0x0F04, 0x0F05, 0x0F06, 0x0F07, 0x0F0A, 0x1001, 0x1002, 0x1003, 0x1004,
];

fn check(type_id: i32, bytes: &[u8]) {
    match Command::parse(type_id, bytes) {
        Ok(command) => {
            assert_eq!(command.command_type() as i32, type_id, "{bytes:02x?}");
            let mut buffer = vec![0u8; bytes.len()];
            let length = command
                .write(&mut buffer)
                .expect("a decoded command encodes");
            assert_eq!(length, bytes.len(), "{bytes:02x?}");
            let again = Command::parse(type_id, &buffer[..length]).expect("encoded bytes decode");
            assert_eq!(again, command, "{bytes:02x?}");
        }
        Err(error) => assert!(REASONS.contains(&error.reason()), "{bytes:02x?}: {error}"),
    }
    match Event::parse(type_id, bytes) {
        Ok(event) => {
            assert_eq!(event.event_type() as i32, type_id, "{bytes:02x?}");
            let mut buffer = vec![0u8; bytes.len()];
            let length = event.write(&mut buffer).expect("a decoded event encodes");
            assert_eq!(length, bytes.len(), "{bytes:02x?}");
            let again = Event::parse(type_id, &buffer[..length]).expect("encoded bytes decode");
            assert_eq!(again, event, "{bytes:02x?}");
        }
        Err(error) => assert!(REASONS.contains(&error.reason()), "{bytes:02x?}: {error}"),
    }
}

fn valid_messages() -> Vec<(i32, Vec<u8>)> {
    let commands = [
        Command::AddPublication {
            client_id: 0,
            correlation_id: 1,
            stream_id: 1001,
            channel: b"cavalry:ipc",
        },
        Command::RemovePublication {
            client_id: 0,
            correlation_id: 2,
            registration_id: 1,
        },
        Command::AddSubscription {
            client_id: 5,
            correlation_id: 6,
            stream_id: -1,
            channel: b"",
        },
        Command::ClientKeepalive {
            client_id: 5,
            correlation_id: 7,
        },
        Command::StartRecording {
            client_id: 5,
            correlation_id: 11,
            stream_id: 1001,
            channel: b"cavalry:ipc",
        },
        Command::StopRecording {
            client_id: 5,
            correlation_id: 12,
            recording_id: 0,
        },
        Command::StartReplay {
            client_id: 5,
            correlation_id: 13,
            recording_id: 0,
            position: -1,
            length: -1,
            replay_stream_id: 2001,
            replay_channel: b"cavalry:ipc",
        },
        Command::StopReplay {
            client_id: 5,
            correlation_id: 14,
            replay_session_id: 13,
        },
    ];
    let events = [
        Event::Error {
            offending_correlation_id: 4,
            error_code: 1,
            error_message: b"channel must be cavalry:ipc",
        },
        Event::AvailableImage {
            correlation_id: 7,
            session_id: -2,
            stream_id: 1001,
            subscriber_registration_id: 6,
            subscriber_position_counter_id: 14,
            log_file_name: b"publications/7.logbuffer",
            source_identity: b"cavalry:ipc",
        },
        Event::PublicationReady {
            correlation_id: 9,
            registration_id: 7,
            session_id: -2,
            stream_id: 1001,
            publication_limit_counter_id: 13,
            channel_status_indicator_id: -1,
            log_file_name: b"publications/7.logbuffer",
        },
        Event::ClientTimeout { client_id: 5 },
        Event::RecordingReady {
            correlation_id: 11,
            recording_id: 0,
            session_id: -2,
            recording_position_counter_id: 16,
            recording_synced_position_counter_id: 17,
        },
        Event::RecordingStopped {
            recording_id: 0,
            stop_position: 4096,
        },
        Event::ReplayReady {
            correlation_id: 13,
            replay_session_id: 13,
            recording_id: 0,
            start_position: 0,
            session_id: -3,
            stream_id: 2001,
        },
        Event::ReplayStopped {
            replay_session_id: 13,
            position: 4096,
            reason: 0,
        },
    ];
    let mut out = Vec::new();
    for command in commands {
        let mut buffer = vec![0u8; 128];
        let length = command.write(&mut buffer).expect("fits");
        out.push((command.command_type() as i32, buffer[..length].to_vec()));
    }
    for event in events {
        let mut buffer = vec![0u8; 128];
        let length = event.write(&mut buffer).expect("fits");
        out.push((event.event_type() as i32, buffer[..length].to_vec()));
    }
    out
}

#[test]
fn random_bytes_under_every_type_never_panic() {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut buf = [0u8; 96];
    for _ in 0..50_000 {
        let len = (rng.next() % 97) as usize;
        for byte in &mut buf[..len] {
            *byte = rng.next() as u8;
        }
        let pick = rng.next() % 24;
        let type_id = match pick {
            0..=10 => COMMAND_TYPES[pick as usize],
            11..=22 => EVENT_TYPES[(pick - 11) as usize],
            _ => rng.next() as i32,
        };
        check(type_id, &buf[..len]);
    }
}

#[test]
fn one_byte_mutations_of_valid_messages_never_panic() {
    for (type_id, message) in valid_messages() {
        check(type_id, &message);
        for offset in 0..message.len() {
            for value in [0x00, 0x01, 0x03, 0x04, 0x7F, 0x80, 0xFF] {
                let mut mutated = message.clone();
                mutated[offset] = value;
                check(type_id, &mutated);
            }
            check(type_id, &message[..offset]);
        }
        let mut longer = message.clone();
        longer.extend_from_slice(&[0, 0, 0, 0]);
        check(type_id, &longer);
    }
}

#[test]
fn every_type_id_is_a_command_an_event_or_rejected() {
    let message = [0u8; 16];
    for type_id in -2..=0x1200 {
        let is_command = CommandType::try_from(type_id).is_ok();
        let is_event = EventType::try_from(type_id).is_ok();
        assert!(!(is_command && is_event), "type {type_id} is both");
        assert_eq!(
            is_command,
            COMMAND_TYPES.contains(&type_id),
            "type {type_id}"
        );
        assert_eq!(is_event, EVENT_TYPES.contains(&type_id), "type {type_id}");
        check(type_id, &message);
    }
}
