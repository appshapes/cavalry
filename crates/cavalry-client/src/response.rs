//! What the conductor hands the user's thread in answer to a command: the event that
//! names the command's correlation id, as an owned value, through the response queue
//! (ADR 0021). One command is in flight per client, so the queue is short, and a
//! response to a correlation id nobody waits for is dropped.

/// The driver's answer to one command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Response {
    /// `publication ready` or `exclusive publication ready`.
    PublicationReady {
        correlation_id: i64,
        registration_id: i64,
        session_id: i32,
        stream_id: i32,
        limit_counter_id: i32,
        log_file_name: String,
        exclusive: bool,
    },
    /// `subscription ready`.
    SubscriptionReady { correlation_id: i64 },
    /// `operation success`.
    Success { correlation_id: i64 },
    /// `recording ready`.
    RecordingReady {
        correlation_id: i64,
        recording_id: i64,
        session_id: i32,
        position_counter_id: i32,
        synced_position_counter_id: i32,
    },
    /// `error`, with Aeron's code and the driver's message.
    Error {
        correlation_id: i64,
        code: i32,
        message: String,
    },
}

impl Response {
    pub(crate) fn correlation_id(&self) -> i64 {
        match self {
            Response::PublicationReady { correlation_id, .. }
            | Response::SubscriptionReady { correlation_id }
            | Response::Success { correlation_id }
            | Response::RecordingReady { correlation_id, .. }
            | Response::Error { correlation_id, .. } => *correlation_id,
        }
    }
}
