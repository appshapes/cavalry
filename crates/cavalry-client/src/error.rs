//! Why a client could not attach, could not be served, or is closed. Every message
//! says what the operator does next and names the page that explains it.

use std::fmt;
use std::path::PathBuf;

use cavalry_proto::client_protocol::{ErrorCode, ProtocolError};
use cavalry_shm::broadcast::BroadcastError;
use cavalry_shm::command_ring::CommandRingError;
use cavalry_shm::counters::CountersError;
use cavalry_shm::log::LogError;

const DOC_DIRECTORY: &str = "docs/reference/client-protocol.md#the-driver-directory";
const DOC_LIVENESS: &str = "docs/reference/client-protocol.md#liveness-and-timeouts";
const DOC_ERRORS: &str = "docs/reference/client-protocol.md#error-codes";
const DOC_SIZES: &str = "docs/reference/client-protocol.md#sizes";
const DOC_BROADCAST: &str = "docs/reference/broadcast-buffer.md#receiving";

/// Why the client is closed: by the user, or by what the conductor found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Closed {
    /// [`crate::Cavalry::close`] was called.
    ByUser,
    /// The driver stored −1 in its heartbeat: it closed.
    DriverClosed,
    /// The driver's heartbeat went stale for the driver timeout: it died or stalled.
    DriverTimeout,
    /// The driver sent `client timeout` for this client: it saw no command for the
    /// client liveness timeout, and removed the client's publications and subscriptions.
    ClientTimeout,
    /// The broadcast buffer lapped this client: events were overwritten unread, so what
    /// the driver did is unknown, and a client that cannot know does not guess.
    Lapped,
    /// The ring or the buffer failed a check of its page, or an event failed a decoding
    /// check (`too-short`, `type`, `string` or `length`): corrupt, not the driver's, or
    /// from a driver of another version.
    Corrupt,
    /// The driver announced or withdrew images faster than a subscription polled: its
    /// image queue was full, and a client that cannot keep its images is not told what it
    /// reads (ADR 0021).
    Overrun,
}

impl Closed {
    /// The reason's name: `by-user`, `driver-closed`, `driver-timeout`,
    /// `client-timeout`, `lapped`, `corrupt` or `overrun`.
    pub const fn reason(self) -> &'static str {
        match self {
            Closed::ByUser => "by-user",
            Closed::DriverClosed => "driver-closed",
            Closed::DriverTimeout => "driver-timeout",
            Closed::ClientTimeout => "client-timeout",
            Closed::Lapped => "lapped",
            Closed::Corrupt => "corrupt",
            Closed::Overrun => "overrun",
        }
    }

    pub(crate) const fn code(self) -> i32 {
        match self {
            Closed::ByUser => 1,
            Closed::DriverClosed => 2,
            Closed::DriverTimeout => 3,
            Closed::ClientTimeout => 4,
            Closed::Lapped => 5,
            Closed::Corrupt => 6,
            Closed::Overrun => 7,
        }
    }

    pub(crate) const fn from_code(code: i32) -> Option<Closed> {
        match code {
            1 => Some(Closed::ByUser),
            2 => Some(Closed::DriverClosed),
            3 => Some(Closed::DriverTimeout),
            4 => Some(Closed::ClientTimeout),
            5 => Some(Closed::Lapped),
            6 => Some(Closed::Corrupt),
            7 => Some(Closed::Overrun),
            _ => None,
        }
    }
}

/// Why a call on the client failed.
#[derive(Debug)]
pub enum ClientError {
    /// No driver directory, or no command ring in it: no driver runs there.
    NotFound {
        /// The directory looked in.
        dir: PathBuf,
    },
    /// The driver at the directory closed, or its heartbeat is stale.
    DriverGone {
        /// The directory.
        dir: PathBuf,
        /// `driver-closed` or `driver-timeout`.
        reason: Closed,
    },
    /// The client is closed: by the user, or by what the conductor found.
    Closed(Closed),
    /// The driver refused a command with an `error` event.
    Refused {
        /// Aeron's code, when this version knows it.
        code: Option<ErrorCode>,
        /// The code as sent.
        raw_code: i32,
        /// The driver's message.
        message: String,
    },
    /// The driver accepted `add subscription`, but this client's conductor already
    /// holds as many subscriptions as the driver allows, so it could not hand the new
    /// one its images. The subscription is removed again, and nothing is read from it.
    SubscriptionsFull {
        /// The bound: the driver's own on subscriptions.
        limit: usize,
    },
    /// The driver accepted `start replay`, but this client's conductor already follows
    /// as many replays as the driver allows at once, so it could not keep the new
    /// one's end for its handle. The replay is stopped again, and nothing is played.
    /// The conductor forgets a replay once it has ended or its handle is dropped, so
    /// this names a client with that many replays live at once.
    ReplaysFull {
        /// The bound: the driver's own on replays.
        limit: usize,
    },
    /// An add or remove command went unanswered for the response timeout.
    ResponseTimeout {
        /// The command, by name.
        command: &'static str,
    },
    /// The command ring refused a write, or failed a check.
    CommandRing(CommandRingError),
    /// The broadcast buffer failed a check.
    Broadcast(BroadcastError),
    /// The counters file failed a check.
    Counters(CountersError),
    /// A command could not be encoded: a channel or a name too long.
    Protocol(ProtocolError),
    /// A log buffer failed a check, or could not be opened.
    Log(LogError),
    /// The driver answered with something this version does not understand, or with an
    /// answer that does not match the command: another log, stream or kind.
    Driver {
        /// What it said.
        what: String,
    },
    /// The operating system refused a step.
    Io {
        /// The step that failed.
        what: &'static str,
        /// The path it failed on.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
}

impl ClientError {
    /// The reason's name: `not-found`, `driver-gone`, `closed`, `refused`,
    /// `subscriptions-full`, `replays-full`, `response-timeout`, `command-ring`,
    /// `broadcast`, `counters`, `protocol`, `log`, `driver` or `io`.
    pub const fn reason(&self) -> &'static str {
        match self {
            ClientError::NotFound { .. } => "not-found",
            ClientError::DriverGone { .. } => "driver-gone",
            ClientError::Closed(_) => "closed",
            ClientError::Refused { .. } => "refused",
            ClientError::SubscriptionsFull { .. } => "subscriptions-full",
            ClientError::ReplaysFull { .. } => "replays-full",
            ClientError::ResponseTimeout { .. } => "response-timeout",
            ClientError::CommandRing(_) => "command-ring",
            ClientError::Broadcast(_) => "broadcast",
            ClientError::Counters(_) => "counters",
            ClientError::Protocol(_) => "protocol",
            ClientError::Log(_) => "log",
            ClientError::Driver { .. } => "driver",
            ClientError::Io { .. } => "io",
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = self.reason();
        match self {
            ClientError::NotFound { dir } => write!(
                f,
                "client refused ({reason}): no driver at {}; start one with `cavalry run`, or set \
                 CAVALRY_DIR to the directory of the driver to use; see {DOC_DIRECTORY}",
                dir.display()
            ),
            ClientError::DriverGone { dir, reason: why } => write!(
                f,
                "client refused ({reason}): the driver at {} is gone ({}); start one with \
                 `cavalry run`, then connect again; see {DOC_LIVENESS}",
                dir.display(),
                why.reason()
            ),
            ClientError::Closed(why) => write!(
                f,
                "client closed ({}): {}; see {DOC_LIVENESS}",
                why.reason(),
                match why {
                    Closed::ByUser => "close was called; connect a new client",
                    Closed::DriverClosed =>
                        "the driver closed; start one with `cavalry run` and connect a new client",
                    Closed::DriverTimeout =>
                        "the driver's heartbeat went stale; check whether it is alive with \
                         `cavalry doctor`, and connect a new client",
                    Closed::ClientTimeout =>
                        "the driver timed this client out: its conductor sent no command for 10 s, \
                         which a paused or starved process does; connect a new client",
                    Closed::Lapped =>
                        "the broadcast buffer lapped this client: events were overwritten before \
                         it read them, so the state of its publications and subscriptions is \
                         unknown; connect a new client, and poll it more often or run fewer clients",
                    Closed::Corrupt =>
                        "a file of the driver directory failed a check of its page, or an event \
                         from the driver failed a decoding check: run `cavalry doctor`, check that \
                         the driver and the client are the same version, then restart the driver, \
                         which remakes its files",
                    Closed::Overrun =>
                        "images were announced faster than a subscription polled, and its queue \
                         filled; connect a new client, and poll each subscription at least every \
                         few hundred image changes",
                }
            ),
            ClientError::Refused {
                code,
                raw_code,
                message,
            } => write!(
                f,
                "command refused ({reason}, error code {raw_code}{}): {message}; see {DOC_ERRORS}",
                code.map_or(String::new(), |code| format!(", {code:?}"))
            ),
            ClientError::SubscriptionsFull { limit } => write!(
                f,
                "command refused ({reason}): this client holds {limit} subscriptions, the \
                 driver's bound; the new one was removed again; close subscriptions this \
                 process no longer polls, or spread them across processes; see {DOC_SIZES}"
            ),
            ClientError::ReplaysFull { limit } => write!(
                f,
                "command refused ({reason}): this client follows {limit} replays at once, the \
                 driver's bound; the new one was stopped again; wait for one of them to end, or \
                 stop one this process no longer needs; see {DOC_SIZES}"
            ),
            ClientError::ResponseTimeout { command } => write!(
                f,
                "command lost ({reason}): no answer to {command} within 10 s; the driver is \
                 stalled or the command was unblocked; run `cavalry doctor`, then try again; see \
                 {DOC_LIVENESS}"
            ),
            ClientError::CommandRing(error) => write!(f, "client refused ({reason}): {error}"),
            ClientError::Broadcast(error) => {
                write!(f, "client refused ({reason}): {error}; see {DOC_BROADCAST}")
            }
            ClientError::Counters(error) => write!(f, "client refused ({reason}): {error}"),
            ClientError::Protocol(error) => write!(f, "command refused ({reason}): {error}"),
            ClientError::Log(error) => write!(f, "client refused ({reason}): {error}"),
            ClientError::Driver { what } => write!(
                f,
                "client refused ({reason}): the driver answered with {what}, which this version \
                 does not understand or did not ask for; a driver newer than the client says \
                 what it cannot read, so upgrade the client to the driver's version; if they \
                 are the same version, the answer does not match its command, which is a bug: \
                 run `cavalry doctor` and report it; see {DOC_ERRORS}"
            ),
            ClientError::Io { what, path, source } => write!(
                f,
                "client refused ({reason}): cannot {what} at {}: {source}; see {DOC_DIRECTORY}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::CommandRing(error) => Some(error),
            ClientError::Broadcast(error) => Some(error),
            ClientError::Counters(error) => Some(error),
            ClientError::Protocol(error) => Some(error),
            ClientError::Log(error) => Some(error),
            ClientError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<CommandRingError> for ClientError {
    fn from(error: CommandRingError) -> Self {
        ClientError::CommandRing(error)
    }
}

impl From<BroadcastError> for ClientError {
    fn from(error: BroadcastError) -> Self {
        ClientError::Broadcast(error)
    }
}

impl From<CountersError> for ClientError {
    fn from(error: CountersError) -> Self {
        ClientError::Counters(error)
    }
}

impl From<ProtocolError> for ClientError {
    fn from(error: ProtocolError) -> Self {
        ClientError::Protocol(error)
    }
}

impl From<LogError> for ClientError {
    fn from(error: LogError) -> Self {
        ClientError::Log(error)
    }
}
