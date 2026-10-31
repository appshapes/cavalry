//! Runs every vector in `spec/vectors/client-protocol.json` against this crate: a valid
//! vector decodes to its fields, those fields encode to the same bytes, and an invalid
//! vector is rejected for the named reason (`spec/README.md`, "Conventions"; ADR 0019).
//! `just conformance` runs this test alone; `just test` runs it with the rest.

use cavalry_proto::client_protocol::{Command, Event};
use serde_json::{Value, json};

const VECTORS: &str = include_str!("../../../spec/vectors/client-protocol.json");

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("vector bytes are hex"))
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("the vectors' strings are UTF-8")
}

/// The decoded command as the vector's `fields` object: the field names of the spec's
/// tables plus `name`.
fn command_fields(command: &Command<'_>) -> Value {
    match *command {
        Command::AddPublication {
            client_id,
            correlation_id,
            stream_id,
            channel,
        } => {
            json!({"name": "add_publication", "client_id": client_id, "correlation_id": correlation_id, "stream_id": stream_id, "channel": text(channel)})
        }
        Command::RemovePublication {
            client_id,
            correlation_id,
            registration_id,
        } => {
            json!({"name": "remove_publication", "client_id": client_id, "correlation_id": correlation_id, "registration_id": registration_id})
        }
        Command::AddExclusivePublication {
            client_id,
            correlation_id,
            stream_id,
            channel,
        } => {
            json!({"name": "add_exclusive_publication", "client_id": client_id, "correlation_id": correlation_id, "stream_id": stream_id, "channel": text(channel)})
        }
        Command::AddSubscription {
            client_id,
            correlation_id,
            stream_id,
            channel,
        } => {
            json!({"name": "add_subscription", "client_id": client_id, "correlation_id": correlation_id, "stream_id": stream_id, "channel": text(channel)})
        }
        Command::RemoveSubscription {
            client_id,
            correlation_id,
            registration_id,
        } => {
            json!({"name": "remove_subscription", "client_id": client_id, "correlation_id": correlation_id, "registration_id": registration_id})
        }
        Command::ClientKeepalive {
            client_id,
            correlation_id,
        } => {
            json!({"name": "client_keepalive", "client_id": client_id, "correlation_id": correlation_id})
        }
        Command::ClientClose {
            client_id,
            correlation_id,
        } => {
            json!({"name": "client_close", "client_id": client_id, "correlation_id": correlation_id})
        }
        Command::StartRecording {
            client_id,
            correlation_id,
            stream_id,
            channel,
        } => {
            json!({"name": "start_recording", "client_id": client_id, "correlation_id": correlation_id, "stream_id": stream_id, "channel": text(channel)})
        }
        Command::StopRecording {
            client_id,
            correlation_id,
            recording_id,
        } => {
            json!({"name": "stop_recording", "client_id": client_id, "correlation_id": correlation_id, "recording_id": recording_id})
        }
        Command::StartReplay {
            client_id,
            correlation_id,
            recording_id,
            position,
            length,
            replay_stream_id,
            replay_channel,
        } => {
            json!({"name": "start_replay", "client_id": client_id, "correlation_id": correlation_id, "recording_id": recording_id, "position": position, "length": length, "replay_stream_id": replay_stream_id, "replay_channel": text(replay_channel)})
        }
        Command::StopReplay {
            client_id,
            correlation_id,
            replay_session_id,
        } => {
            json!({"name": "stop_replay", "client_id": client_id, "correlation_id": correlation_id, "replay_session_id": replay_session_id})
        }
        Command::TruncateRecording {
            client_id,
            correlation_id,
            recording_id,
            position,
        } => {
            json!({"name": "truncate_recording", "client_id": client_id, "correlation_id": correlation_id, "recording_id": recording_id, "position": position})
        }
        Command::PurgeSegments {
            client_id,
            correlation_id,
            recording_id,
            new_start_position,
        } => {
            json!({"name": "purge_segments", "client_id": client_id, "correlation_id": correlation_id, "recording_id": recording_id, "new_start_position": new_start_position})
        }
        Command::PurgeRecording {
            client_id,
            correlation_id,
            recording_id,
        } => {
            json!({"name": "purge_recording", "client_id": client_id, "correlation_id": correlation_id, "recording_id": recording_id})
        }
    }
}

/// The decoded event as the vector's `fields` object.
fn event_fields(event: &Event<'_>) -> Value {
    match *event {
        Event::Error {
            offending_correlation_id,
            error_code,
            error_message,
        } => {
            json!({"name": "error", "offending_correlation_id": offending_correlation_id, "error_code": error_code, "error_message": text(error_message)})
        }
        Event::AvailableImage {
            correlation_id,
            session_id,
            stream_id,
            subscriber_registration_id,
            subscriber_position_counter_id,
            log_file_name,
            source_identity,
        } => {
            json!({"name": "available_image", "correlation_id": correlation_id, "session_id": session_id, "stream_id": stream_id, "subscriber_registration_id": subscriber_registration_id, "subscriber_position_counter_id": subscriber_position_counter_id, "log_file_name": text(log_file_name), "source_identity": text(source_identity)})
        }
        Event::PublicationReady {
            correlation_id,
            registration_id,
            session_id,
            stream_id,
            publication_limit_counter_id,
            log_file_name,
        } => {
            json!({"name": "publication_ready", "correlation_id": correlation_id, "registration_id": registration_id, "session_id": session_id, "stream_id": stream_id, "publication_limit_counter_id": publication_limit_counter_id, "log_file_name": text(log_file_name)})
        }
        Event::OperationSuccess { correlation_id } => {
            json!({"name": "operation_success", "correlation_id": correlation_id})
        }
        Event::UnavailableImage {
            correlation_id,
            subscriber_registration_id,
            stream_id,
            channel,
        } => {
            json!({"name": "unavailable_image", "correlation_id": correlation_id, "subscriber_registration_id": subscriber_registration_id, "stream_id": stream_id, "channel": text(channel)})
        }
        Event::ExclusivePublicationReady {
            correlation_id,
            registration_id,
            session_id,
            stream_id,
            publication_limit_counter_id,
            log_file_name,
        } => {
            json!({"name": "exclusive_publication_ready", "correlation_id": correlation_id, "registration_id": registration_id, "session_id": session_id, "stream_id": stream_id, "publication_limit_counter_id": publication_limit_counter_id, "log_file_name": text(log_file_name)})
        }
        Event::SubscriptionReady { correlation_id } => {
            json!({"name": "subscription_ready", "correlation_id": correlation_id})
        }
        Event::ClientTimeout { client_id } => {
            json!({"name": "client_timeout", "client_id": client_id})
        }
        Event::RecordingReady {
            correlation_id,
            recording_id,
            session_id,
            recording_position_counter_id,
            recording_synced_position_counter_id,
        } => {
            json!({"name": "recording_ready", "correlation_id": correlation_id, "recording_id": recording_id, "session_id": session_id, "recording_position_counter_id": recording_position_counter_id, "recording_synced_position_counter_id": recording_synced_position_counter_id})
        }
        Event::RecordingStopped {
            recording_id,
            stop_position,
        } => {
            json!({"name": "recording_stopped", "recording_id": recording_id, "stop_position": stop_position})
        }
        Event::ReplayReady {
            correlation_id,
            replay_session_id,
            recording_id,
            start_position,
            session_id,
            stream_id,
        } => {
            json!({"name": "replay_ready", "correlation_id": correlation_id, "replay_session_id": replay_session_id, "recording_id": recording_id, "start_position": start_position, "session_id": session_id, "stream_id": stream_id})
        }
        Event::ReplayStopped {
            replay_session_id,
            position,
            reason,
        } => {
            json!({"name": "replay_stopped", "replay_session_id": replay_session_id, "position": position, "reason": reason})
        }
    }
}

#[test]
fn every_vector_in_spec_vectors_client_protocol_json() {
    let doc: Value = serde_json::from_str(VECTORS).expect("client-protocol.json parses");
    assert_eq!(doc["version"], 3, "the vectors are for version 3");
    let vectors = doc["vectors"].as_array().expect("vectors is an array");
    assert!(
        vectors.len() >= 80,
        "client-protocol.py writes at least 80 vectors"
    );

    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        let region = vector["region"].as_str().expect("region");
        let type_id = i32::try_from(vector["type"].as_i64().expect("type")).expect("type fits i32");
        let bytes = hex(vector["bytes"].as_str().expect("bytes"));

        // Decode, or report the named reason.
        let (fields, written) = match region {
            "command" => match Command::parse(type_id, &bytes) {
                Ok(command) => {
                    let mut buffer = vec![0u8; bytes.len()];
                    let length = command
                        .write(&mut buffer)
                        .expect("a decoded command encodes");
                    assert_eq!(command.command_type() as i32, type_id, "{name}: type");
                    assert_eq!(
                        command.encoded_length(),
                        Ok(length),
                        "{name}: encoded_length"
                    );
                    (Ok(command_fields(&command)), buffer[..length].to_vec())
                }
                Err(error) => (Err(error), Vec::new()),
            },
            "event" => match Event::parse(type_id, &bytes) {
                Ok(event) => {
                    let mut buffer = vec![0u8; bytes.len()];
                    let length = event.write(&mut buffer).expect("a decoded event encodes");
                    assert_eq!(event.event_type() as i32, type_id, "{name}: type");
                    assert_eq!(event.encoded_length(), Ok(length), "{name}: encoded_length");
                    (Ok(event_fields(&event)), buffer[..length].to_vec())
                }
                Err(error) => (Err(error), Vec::new()),
            },
            other => panic!("{name}: unknown region {other}"),
        };

        if let Some(reason) = vector.get("error") {
            let reason = reason.as_str().expect("error is a string");
            match fields {
                Err(error) => assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}"),
                Ok(fields) => panic!("{name}: accepted, expected {reason}: {fields}"),
            }
            continue;
        }

        let fields = fields.unwrap_or_else(|error| panic!("{name}: rejected: {error}"));
        assert_eq!(fields, vector["fields"], "{name}: fields");
        assert_eq!(
            written, bytes,
            "{name}: the fields encode to different bytes"
        );
    }
}
