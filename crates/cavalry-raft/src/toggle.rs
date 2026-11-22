//! The control toggle: how a command on a member's host asks that member for a
//! snapshot (`spec/cluster.md`, "Snapshots", "The control toggle"; ADR 0147). In
//! Aeron, this is the cluster control toggle, which `ClusterTool` writes.
//!
//! The command adds a counter, `cluster.control_toggle`, keyed by the member's id and
//! the toggle's id, and stores 4, snapshot, in it. The command is the counter's one
//! writer. A leader finds the counter by reading its driver's counters file, a few
//! metadata records at a time: [`CONTROL_TOGGLE_LOOK_RECORDS`] of them, no more often
//! than [`CONTROL_TOGGLE_LOOK_INTERVAL_NS`], in a ring. The file has no sign that a
//! counter was added, so the leader reads; it never reads the whole file at once, and
//! a member that does not lead reads nothing.
//!
//! A leader takes each toggle once: it takes none whose id is at or below the id of
//! the last it took. A toggle's id is its command's client id, and a driver gives its
//! clients rising ids, so a toggle that stays, of a command that did not end, is
//! taken once and holds no other toggle back. What the member took, and the log
//! position of the snapshot it asked for, are two gauges the command reads: the
//! position is stored first and the id after it, both with release stores, so a
//! command that loads its own id with an acquire load reads its own toggle's
//! position.

use cavalry_client::{Cavalry, ClientError, Counter};
use cavalry_proto::cluster::{
    CONTROL_TOGGLE_LOOK_INTERVAL_NS, CONTROL_TOGGLE_LOOK_RECORDS, CONTROL_TOGGLE_SNAPSHOT,
    NULL_VALUE, control_toggle_id,
};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_CONTROL_TOGGLE, CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION, CLUSTER_CONTROL_TOGGLE_TAKEN,
};

/// A control toggle a look found, by its id: the client id of the command that added
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Found(i64);

/// A member's side of the control toggle: where its look is in the counters file, the
/// id of the last toggle it took, and its two gauges.
#[derive(Debug)]
pub(crate) struct ControlToggle {
    member_id: i32,
    /// The record the next look begins at.
    next: u32,
    /// The clock of the last look, `None` before the first.
    looked_at_ns: Option<u64>,
    /// The id of the toggle the member took last, −1 for none: what
    /// `cluster.control_toggle_taken` holds once the member has stored it. No toggle
    /// with an id at or below it is taken.
    taken: i64,
    /// `cluster.control_toggle_snapshot_position`, then `cluster.control_toggle_taken`.
    gauges: [Counter; 2],
}

impl ControlToggle {
    /// Adds the member's two gauges, each −1: it has taken no toggle.
    pub(crate) fn open(client: &mut Cavalry, member_id: i32) -> Result<Self, ClientError> {
        let mut add = |id: i32, name: &str| {
            let counter =
                client.add_counter(id, &member_id.to_le_bytes(), &format!("{name} {member_id}"))?;
            counter.store_release(NULL_VALUE)?;
            Ok::<_, ClientError>(counter)
        };
        let position = add(
            CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION.0,
            "cluster.control_toggle_snapshot_position",
        )?;
        let taken = add(
            CLUSTER_CONTROL_TOGGLE_TAKEN.0,
            "cluster.control_toggle_taken",
        )?;
        Ok(Self {
            member_id,
            next: 0,
            looked_at_ns: None,
            taken: NULL_VALUE,
            gauges: [position, taken],
        })
    }

    /// One look, for a member that leads: a toggle of this member that asks for a
    /// snapshot and whose id is above the last it took, when the look meets one.
    /// `None` when the look's interval has not passed since the last, and when the
    /// records read hold none.
    ///
    /// A toggle that is found is found again by the next look, which begins at its
    /// record, until the member takes it or it goes: a leader that cannot take one
    /// yet reads one record a look for it.
    pub(crate) fn look(&mut self, counters: &CountersReader, now_ns: u64) -> Option<Found> {
        if self
            .looked_at_ns
            .is_some_and(|at| now_ns.saturating_sub(at) < CONTROL_TOGGLE_LOOK_INTERVAL_NS)
        {
            return None;
        }
        self.looked_at_ns = Some(now_ns);
        let capacity = counters.capacity();
        for _ in 0..CONTROL_TOGGLE_LOOK_RECORDS.min(capacity) {
            let counter = if self.next < capacity { self.next } else { 0 };
            if let Some(toggle) = self.toggle_at(counters, counter)
                && toggle > self.taken
                && matches!(
                    counters.read_acquire(counter),
                    Ok(CONTROL_TOGGLE_SNAPSHOT)
                )
                // The record once more, after the value: a counter the driver freed and
                // gave to another client between the two reads is not this toggle.
                && self.toggle_at(counters, counter) == Some(toggle)
            {
                // The ring stays at this record: the next look reads it first.
                self.next = counter;
                return Some(Found(toggle));
            }
            self.next = counter.saturating_add(1);
        }
        None
    }

    /// The id of the toggle of this member that counter `counter` holds, `None` when
    /// it holds another counter, another member's toggle or none.
    fn toggle_at(&self, counters: &CountersReader, counter: u32) -> Option<i64> {
        let record = counters.record(counter).ok().flatten()?;
        if record.type_id() != CLUSTER_CONTROL_TOGGLE.0 {
            return None;
        }
        control_toggle_id(record.key(), self.member_id)
    }

    /// The member took `found`: it appended the cluster action, and the snapshot it
    /// asked for is of `snapshot_position`, `None` when the log's position could not
    /// be read after the append. The toggle is not taken again either way. With a
    /// position, the position is stored first and the toggle's id after it, each with
    /// a release store, so that the command that reads its own id reads its own
    /// toggle's position (`spec/cluster.md`, "The control toggle"); with none, nothing
    /// is stored, and the command's wait ends at its timeout. A store that a closed
    /// client refuses is of a member that is closing.
    pub(crate) fn took(&mut self, found: Found, snapshot_position: Option<i64>) {
        let Found(toggle) = found;
        self.taken = toggle;
        if let Some(snapshot_position) = snapshot_position {
            let [position, taken] = &self.gauges;
            let _ = position.store_release(snapshot_position);
            let _ = taken.store_release(toggle);
        }
    }
}
