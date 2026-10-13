//! [`Cavalry`], a process's attachment to the driver (`spec/client-protocol.md`, "The
//! driver directory"; ADR 0021).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use cavalry_proto::client_protocol::Command;
use cavalry_shm::broadcast::BroadcastBuffer;
use cavalry_shm::command_ring::layout::DRIVER_CLOSED;
use cavalry_shm::command_ring::{CommandRing, Producer};
use cavalry_shm::counters::CountersReader;

use crate::conductor::{COMMAND_SCRATCH, Conductor, DRIVER_TIMEOUT, Shared, nanos, now_nanos};
use crate::error::{ClientError, Closed};

/// The environment variable that names the driver directory (`config.toml`, `dir`;
/// ADR 0020). Clients read the same key the driver does.
pub const DIR_ENV: &str = "CAVALRY_DIR";

/// A client: one process's attachment to one driver. [`Cavalry::connect`] opens the
/// driver directory's files, takes a client id and starts the client conductor thread,
/// which keeps the client alive with the driver and reads every event. Publications and
/// subscriptions are added from here. [`Cavalry::close`] tells the driver and stops the
/// conductor; dropping a client does the same.
pub struct Cavalry {
    dir: PathBuf,
    client_id: i64,
    shared: Arc<Shared>,
    producer: Producer,
    counters: CountersReader,
    conductor: Option<JoinHandle<()>>,
    scratch: Vec<u8>,
}

impl Cavalry {
    /// The directory a client looks in when none is given: `CAVALRY_DIR` when it is set
    /// and not empty, else the driver's default, `/dev/shm/cavalry-<user>` on Linux
    /// when `/dev/shm` exists and `<tmp>/cavalry-<user>` otherwise
    /// (`spec/client-protocol.md`, "The driver directory"; ADR 0020).
    pub fn default_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os(DIR_ENV).filter(|dir| !dir.is_empty()) {
            return PathBuf::from(dir);
        }
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "default".to_string());
        let name = format!("cavalry-{user}");
        let shm = PathBuf::from("/dev/shm");
        if cfg!(target_os = "linux") && shm.is_dir() {
            shm.join(name)
        } else {
            std::env::temp_dir().join(name)
        }
    }

    /// Connects to the driver at [`Cavalry::default_dir`].
    pub fn connect() -> Result<Self, ClientError> {
        Self::connect_to(Self::default_dir())
    }

    /// Connects to the driver at `dir`: opens the command ring writable and checks the
    /// heartbeat, the broadcast buffer read-only at its latest record, and the counters
    /// file read-only; takes a client id from the ring's correlation id counter; sends
    /// the first keepalive, so the driver knows the client; and starts the conductor.
    /// A directory with no command ring is [`ClientError::NotFound`]; a driver that
    /// closed or whose heartbeat is stale is [`ClientError::DriverGone`].
    pub fn connect_to(dir: impl AsRef<Path>) -> Result<Self, ClientError> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.join("command-ring").is_file() {
            return Err(ClientError::NotFound { dir });
        }
        let ring = CommandRing::open(&dir, true)?;
        let now = now_nanos();
        let heartbeat = ring.consumer_heartbeat();
        if heartbeat == DRIVER_CLOSED {
            return Err(ClientError::DriverGone {
                dir,
                reason: Closed::DriverClosed,
            });
        }
        if now.saturating_sub(heartbeat) > nanos(DRIVER_TIMEOUT) {
            return Err(ClientError::DriverGone {
                dir,
                reason: Closed::DriverTimeout,
            });
        }
        let mut producer = ring.producer(DRIVER_TIMEOUT)?;
        let conductor_producer = ring.producer(DRIVER_TIMEOUT)?;
        let client_id = producer.next_correlation_id();
        let broadcast = BroadcastBuffer::open(&dir, false)?;
        let receiver = broadcast.receiver()?;
        let counters = CountersReader::open(&dir)?;

        let mut scratch = vec![0u8; COMMAND_SCRATCH];
        let keepalive = Command::ClientKeepalive {
            client_id,
            correlation_id: producer.next_correlation_id(),
        };
        let length = keepalive.write(&mut scratch)?;
        producer.write(
            keepalive.command_type() as i32,
            &scratch[..length],
            now_nanos(),
        )?;

        let shared = Arc::new(Shared::new(now_nanos()));
        let conductor = Conductor::new(
            ring,
            conductor_producer,
            receiver,
            Arc::clone(&shared),
            client_id,
        );
        let handle = std::thread::Builder::new()
            .name("cavalry-client-conductor".to_string())
            .spawn(move || conductor.run())
            .map_err(|source| ClientError::Io {
                what: "start the client conductor thread",
                path: dir.clone(),
                source,
            })?;
        Ok(Self {
            dir,
            client_id,
            shared,
            producer,
            counters,
            conductor: Some(handle),
            scratch,
        })
    }

    /// The driver directory this client is attached to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The client id the driver knows this client by.
    pub fn client_id(&self) -> i64 {
        self.client_id
    }

    /// Why the client is closed, or `None` while it is open. A client closes when
    /// [`Cavalry::close`] is called, and when the conductor finds the driver gone, this
    /// client timed out, or the broadcast buffer lapped.
    pub fn closed(&self) -> Option<Closed> {
        self.shared.closed()
    }

    /// Whether the client is closed.
    pub fn is_closed(&self) -> bool {
        self.closed().is_some()
    }

    /// The driver's counters file, read-only: what `doctor` reads, from the client.
    pub fn counters(&self) -> &CountersReader {
        &self.counters
    }

    /// Sends `client close`, so the driver removes this client's publications and
    /// subscriptions now rather than at the liveness timeout, and stops the conductor.
    /// A driver that is gone cannot be told, and that is not an error here: the client
    /// is closed either way, and [`Cavalry::closed`] before the call said why.
    pub fn close(mut self) -> Result<(), ClientError> {
        self.shutdown();
        Ok(())
    }

    fn shutdown(&mut self) {
        if self.shared.close(Closed::ByUser) {
            let command = Command::ClientClose {
                client_id: self.client_id,
                correlation_id: self.producer.next_correlation_id(),
            };
            if let Ok(length) = command.write(&mut self.scratch) {
                let _ = self.producer.write(
                    command.command_type() as i32,
                    &self.scratch[..length],
                    now_nanos(),
                );
            }
        }
        if let Some(handle) = self.conductor.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Cavalry {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for Cavalry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cavalry")
            .field("dir", &self.dir)
            .field("client_id", &self.client_id)
            .field("closed", &self.closed())
            .finish()
    }
}
