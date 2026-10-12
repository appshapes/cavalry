//! The property tests/client_protocol_no_panic.rs checks on every commit, under
//! libFuzzer for longer: under any type id, any bytes either decode to a command or an
//! event that encodes to the same length and decodes again to the same value, or are
//! rejected with one of the spec's four reasons, and nothing panics. The type id is
//! the first four bytes of the input; the message is the rest.
#![no_main]

use cavalry_proto::client_protocol::{Command, Event};
use libfuzzer_sys::fuzz_target;

const REASONS: [&str; 4] = ["too-short", "type", "string", "length"];

fuzz_target!(|data: &[u8]| {
    let Some((head, message)) = data.split_at_checked(4) else {
        return;
    };
    let type_id = i32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    match Command::parse(type_id, message) {
        Ok(command) => {
            let mut buffer = vec![0u8; message.len()];
            let length = command.write(&mut buffer).expect("a decoded command encodes");
            assert_eq!(length, message.len());
            assert_eq!(Command::parse(type_id, &buffer[..length]), Ok(command));
        }
        Err(error) => assert!(REASONS.contains(&error.reason())),
    }
    match Event::parse(type_id, message) {
        Ok(event) => {
            let mut buffer = vec![0u8; message.len()];
            let length = event.write(&mut buffer).expect("a decoded event encodes");
            assert_eq!(length, message.len());
            assert_eq!(Event::parse(type_id, &buffer[..length]), Ok(event));
        }
        Err(error) => assert!(REASONS.contains(&error.reason())),
    }
});
