//! [`Counter`]: a counter a client added and owns (`spec/client-protocol.md`,
//! "Commands", add counter; ADR 0062). In Aeron, this is `Counter`, from
//! `Aeron.addCounter` (`docs/glossary.md`).

use std::fmt;
use std::sync::Arc;

use cavalry_proto::client_protocol::Command;
use cavalry_shm::command_ring::Producer;
use cavalry_shm::counters::Counter as Cell;

use crate::conductor::{Shared, epoch_time};
use crate::error::ClientError;

/// A counter this client added with [`Cavalry::add_counter`](crate::Cavalry::add_counter):
/// a record of the driver's counters file with the type id, the key and the label the
/// client gave, whose value this handle alone writes. `cavalry doctor`, and any client
/// that maps the counters file, reads it.
///
/// The driver frees the counter when the handle is closed or dropped, and when the
/// client closes or is timed out; the record may then be allocated to another counter.
/// So every store first checks that the client is open, one acquire load, and stores
/// nothing once it is closed. Nothing here allocates or locks.
pub struct Counter {
    cell: Cell,
    shared: Arc<Shared>,
    producer: Producer,
    client_id: i64,
    registration_id: i64,
    type_id: i32,
    /// Whether `remove counter` was sent, so that `close` and `Drop` send it once.
    removed: bool,
    scratch: Vec<u8>,
}

impl Counter {
    pub(crate) fn new(
        cell: Cell,
        shared: Arc<Shared>,
        producer: Producer,
        client_id: i64,
        registration_id: i64,
        type_id: i32,
    ) -> Self {
        Self {
            cell,
            shared,
            producer,
            client_id,
            registration_id,
            type_id,
            removed: false,
            // `remove counter` is 24 bytes: the two ids and the registration id.
            scratch: vec![0u8; 24],
        }
    }

    /// The id under which the driver knows this counter: the add command's correlation
    /// id, which `unavailable counter` names when the counter goes.
    pub fn registration_id(&self) -> i64 {
        self.registration_id
    }

    /// The record of the counters file the driver allocated: what a reader of the file
    /// finds the value by.
    pub fn id(&self) -> u32 {
        self.cell.id()
    }

    /// The counter's type id: the `id` of its entry in `counters.toml`.
    pub fn type_id(&self) -> i32 {
        self.type_id
    }

    /// Stores `value` with a release store: for a gauge another process branches on, a
    /// position, which that process loads with an acquire load
    /// (`spec/counters-file.md`, "Value slot"). [`ClientError::Closed`] when the client is
    /// closed, and then nothing is stored.
    pub fn store_release(&self, value: i64) -> Result<(), ClientError> {
        self.open()?;
        self.cell.store_release(value);
        Ok(())
    }

    /// Stores `value` with a relaxed store: for a gauge nobody branches on.
    /// [`ClientError::Closed`] when the client is closed, and then nothing is stored.
    pub fn set(&self, value: i64) -> Result<(), ClientError> {
        self.open()?;
        self.cell.set(value);
        Ok(())
    }

    /// Adds one, relaxed: for a counter of a state transition. [`ClientError::Closed`]
    /// when the client is closed, and then nothing is added.
    pub fn increment(&self) -> Result<(), ClientError> {
        self.open()?;
        self.cell.increment();
        Ok(())
    }

    /// Adds `n`, relaxed. [`ClientError::Closed`] when the client is closed, and then
    /// nothing is added.
    pub fn add(&self, n: i64) -> Result<(), ClientError> {
        self.open()?;
        self.cell.add(n);
        Ok(())
    }

    /// The value, with an acquire load.
    pub fn get(&self) -> i64 {
        self.cell.load_acquire()
    }

    /// Whether the client this counter belongs to is closed, so that every store from
    /// here is [`ClientError::Closed`]. [`Counter::close`] consumes the handle, so a counter the
    /// caller still holds is closed only with its client.
    pub fn is_closed(&self) -> bool {
        self.shared.closed().is_some()
    }

    /// Sends `remove counter`: the driver tells every client the counter is going and
    /// frees its record. A driver that is gone cannot be told, and that is not an error
    /// here. Dropping the handle does the same.
    pub fn close(mut self) {
        self.remove();
    }

    fn open(&self) -> Result<(), ClientError> {
        match self.shared.closed() {
            Some(reason) => Err(ClientError::Closed(reason)),
            None => Ok(()),
        }
    }

    fn remove(&mut self) {
        if self.removed {
            return;
        }
        self.removed = true;
        let command = Command::RemoveCounter {
            client_id: self.client_id,
            correlation_id: self.producer.next_correlation_id(),
            registration_id: self.registration_id,
        };
        if let Ok(length) = command.write(&mut self.scratch) {
            let _ = self.producer.write(
                command.command_type() as i32,
                &self.scratch[..length],
                epoch_time(),
            );
        }
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        self.remove();
    }
}

impl fmt::Debug for Counter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Counter")
            .field("registration_id", &self.registration_id)
            .field("id", &self.id())
            .field("type_id", &self.type_id)
            .field("closed", &self.is_closed())
            .finish()
    }
}
