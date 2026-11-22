//! `cavalry snapshot`: asks the cluster member on this host's driver for a snapshot,
//! and waits until that member lists it (`spec/cluster.md`, "Snapshots", "The control
//! toggle"; ADR 0147). In Aeron, this is `ClusterTool snapshot`.
//!
//! The command is a client of the member's driver. It adds a counter, the control
//! toggle, keyed by the member's id and its own client id, and stores 4 in it. The
//! member, while it leads, finds the toggle, appends the cluster action that asks
//! every member for a snapshot, and says what it took in two gauges. The command
//! reads its answer there and in the member's `cluster.snapshot_position`, and
//! removes its toggle when it ends, however it ends.

use std::path::Path;
use std::time::{Duration, Instant};

use cavalry::{Cavalry, Counter};
use cavalry_proto::cluster::{
    CONTROL_TOGGLE_SNAPSHOT, CONTROL_TOGGLE_TIMEOUT_NS, control_toggle_key,
};
use cavalry_shm::counters::generated::{
    CLUSTER_CONTROL_TOGGLE, CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION, CLUSTER_CONTROL_TOGGLE_TAKEN,
    CLUSTER_ELECTION_STATE, CLUSTER_LEADER_MEMBER_ID, CLUSTER_ROLE, CLUSTER_SNAPSHOT_POSITION,
    CLUSTER_SNAPSHOTS_FAILED,
};
use cavalry_shm::counters::{CounterId, CountersReader};

/// Where the command and what it reports are explained.
const DOC: &str = "docs/how-to/ask-for-a-snapshot.md#when-it-does-not-work";

/// `cluster.role` of a member that leads, and `cluster.election_state` of one whose
/// election has closed (`spec/cluster.md`, "Counters").
const LEADER: i64 = 2;
const ELECTION_CLOSED: i64 = 17;

/// How often the command reads the member's counters while it waits.
const POLL: Duration = Duration::from_millis(1);

/// How many times the command attaches for a client id above the id of the toggle
/// the member took last, before it says that no id of this driver's is.
const ATTACHES: usize = 3;

/// What the member answered: its id, the log position of the snapshot it asked for,
/// and the position of the snapshot it lists, which is that one or, where that one
/// was given up and a later one taken, a later one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) member_id: i32,
    pub(crate) asked_position: i64,
    pub(crate) log_position: i64,
}

/// How far the command had come when it found that the member does not lead.
#[derive(Clone, Copy)]
enum When {
    /// No toggle was added yet.
    Before,
    /// The toggle was added, and the member had taken none at or above it.
    Waiting,
    /// The member took a toggle and asked for a snapshot of this log position.
    Taken(i64),
}

/// The counters that say whether the member leads, each by its id in the counters
/// file: its role, its election state and its leader, the first a member adds.
struct Roles {
    id: i32,
    role: u32,
    election_state: u32,
    leader: u32,
}

/// The counters of the member on a driver that the command reads, each by its id in
/// the counters file, found once.
struct Member {
    roles: Roles,
    taken: u32,
    position: u32,
    snapshot_position: u32,
    snapshots_failed: u32,
}

/// The id of the counter of `type_id` whose key begins with `key`, the first in the
/// file.
fn find(counters: &CountersReader, type_id: CounterId, key: &[u8]) -> Option<u32> {
    (0..counters.capacity()).find(|id| {
        matches!(counters.record(*id), Ok(Some(record))
            if record.type_id() == type_id.0 && record.key().starts_with(key))
    })
}

/// The value of member `id`'s counter `counter` of `type_id`, with an acquire load,
/// `None` when the record is no longer that counter: the member has gone.
fn load(counters: &CountersReader, id: i32, counter: u32, type_id: CounterId) -> Option<i64> {
    let value = counters.read_acquire(counter).ok()?;
    matches!(counters.record(counter), Ok(Some(record))
        if record.type_id() == type_id.0 && record.key().starts_with(&id.to_le_bytes()))
    .then_some(value)
}

/// The message when member `id` went while the command read its counters. `when`
/// says how far the command had come.
fn gone(id: i32, when: When) -> String {
    format!(
        "member {id} went while the command read its counters{}. Run `cavalry doctor` on \
         this host, start the member again if it stopped, and run `cavalry snapshot` on \
         the host of the member that leads; see {DOC}",
        match when {
            When::Before => ", so nothing was asked",
            When::Waiting => ", and the request was removed",
            When::Taken(_) => ", after it had taken the request",
        }
    )
}

/// The message when member `id` does not lead with its election closed. `leader` is
/// the member that leads, where the member names one with its election closed, and
/// `when` says how far the command had come.
fn does_not_lead(id: i32, when: When, leader: Option<i64>) -> String {
    let what = match when {
        When::Before => "does not lead, so nothing was asked".to_string(),
        When::Waiting => {
            "stopped leading while the request waited, and the request was removed".to_string()
        }
        When::Taken(position) => format!(
            "took the request, asked every member for a snapshot of log position \
             {position}, and stopped leading before it listed its own. That snapshot is \
             taken where the cluster committed the request, and is not where the next \
             leader's log does not hold it"
        ),
    };
    match leader {
        Some(leader) => format!(
            "member {id} {what}: member {leader} leads. Run `cavalry snapshot` on member \
             {leader}'s host, with its driver directory; see {DOC}"
        ),
        None => format!(
            "member {id} {what}: the cluster is holding an election. Run `cavalry doctor` \
             until its `cluster-member` note says the election closed and names the \
             leader, then run `cavalry snapshot` on that member's host; see {DOC}"
        ),
    }
}

impl Roles {
    /// The message when the member does not lead with its election closed, `None`
    /// when it does. `when` says how far the command had come when this was read.
    fn not_leading(&self, counters: &CountersReader, when: When) -> Option<String> {
        let id = self.id;
        let (Some(role), Some(state)) = (
            load(counters, id, self.role, CLUSTER_ROLE),
            load(counters, id, self.election_state, CLUSTER_ELECTION_STATE),
        ) else {
            return Some(gone(id, when));
        };
        if role == LEADER && state == ELECTION_CLOSED {
            return None;
        }
        let leader = load(counters, id, self.leader, CLUSTER_LEADER_MEMBER_ID)
            .filter(|leader| *leader >= 0 && *leader != i64::from(id) && state == ELECTION_CLOSED);
        Some(does_not_lead(id, when, leader))
    }
}

impl Member {
    /// The member on the driver, by its `cluster.role`, and its counters; the message
    /// to print when the driver hosts none, when the member does not lead with its
    /// election closed, or when it serves no control toggle.
    ///
    /// Whether the member leads is read first, and its other counters then, once. A
    /// member adds its role, its election state and its leader at its start, then the
    /// counters of its snapshots, then the two gauges of the control toggle, all
    /// before its first duty cycle, and it begins as a follower in its election's
    /// `Init` (`crates/cavalry-raft/src/module.rs`, `ConsensusModule::start`). So a
    /// member that is starting does not lead, and is told so. One that leads with its
    /// election closed has added every counter of its build, and one with no gauge
    /// then is of a build before the control toggle, which is said at once
    /// (`spec/cluster.md`, "The control toggle").
    fn find(counters: &CountersReader, dir: &Path) -> Result<Self, String> {
        let no_member = || {
            format!(
                "the driver in {} hosts no cluster member, so there is nobody to ask: run \
                 `cavalry snapshot --dir <the driver directory of the member that leads>` on \
                 that member's host; `cavalry doctor` there names the member in its \
                 `cluster-member` note; see {DOC}",
                dir.display()
            )
        };
        let role = find(counters, CLUSTER_ROLE, &[]).ok_or_else(no_member)?;
        let id = counters
            .record(role)
            .ok()
            .flatten()
            .and_then(|record| record.key().first_chunk::<4>().copied())
            .map(i32::from_le_bytes)
            .ok_or_else(no_member)?;
        let key = id.to_le_bytes();
        let own = |type_id: CounterId| find(counters, type_id, &key);
        let Some((election_state, leader)) =
            own(CLUSTER_ELECTION_STATE).zip(own(CLUSTER_LEADER_MEMBER_ID))
        else {
            // A member adds these two just after its role: it is starting, and its
            // election has not closed, or it went.
            return Err(match load(counters, id, role, CLUSTER_ROLE) {
                Some(_) => does_not_lead(id, When::Before, None),
                None => gone(id, When::Before),
            });
        };
        let roles = Roles {
            id,
            role,
            election_state,
            leader,
        };
        if let Some(message) = roles.not_leading(counters, When::Before) {
            return Err(message);
        }
        let gauges =
            own(CLUSTER_CONTROL_TOGGLE_TAKEN).zip(own(CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION));
        let snapshots = own(CLUSTER_SNAPSHOT_POSITION).zip(own(CLUSTER_SNAPSHOTS_FAILED));
        match (gauges, snapshots) {
            (Some((taken, position)), Some((snapshot_position, snapshots_failed))) => Ok(Self {
                roles,
                taken,
                position,
                snapshot_position,
                snapshots_failed,
            }),
            // A member that went, or stopped leading, since the read above is told so.
            // One that still leads and has no gauge is of a build before the control
            // toggle. A build with the gauges adds the counters of its snapshots before
            // them, so a member with its gauges and without those is going.
            (gauges, _) => Err(roles
                .not_leading(counters, When::Before)
                .unwrap_or_else(|| match gauges {
                    None => format!(
                        "member {id} is of a build that takes no request for a snapshot: it \
                         leads, with its election closed, and has no \
                         `cluster.control_toggle_taken` counter. Nothing was asked. Upgrade \
                         the member, or wait for the snapshot it asks for by itself each \
                         256 MiB of log; see {DOC}"
                    ),
                    Some(_) => gone(id, When::Before),
                })),
        }
    }

    /// The member's id in its cluster.
    fn id(&self) -> i32 {
        self.roles.id
    }

    /// The value of the member's counter `counter` of `type_id`, with an acquire load,
    /// `None` when the record is no longer that counter: the member has gone.
    fn load(&self, counters: &CountersReader, counter: u32, type_id: CounterId) -> Option<i64> {
        load(counters, self.roles.id, counter, type_id)
    }

    /// [`Roles::not_leading`], of this member.
    fn not_leading(&self, counters: &CountersReader, when: When) -> Option<String> {
        self.roles.not_leading(counters, when)
    }
}

/// Asks the member on the driver in `dir` for a snapshot and waits until it lists
/// one that it asked for after this command began; the message to print otherwise.
pub(crate) fn ask(dir: &Path) -> Result<Listed, String> {
    ask_within(dir, Duration::from_nanos(CONTROL_TOGGLE_TIMEOUT_NS))
}

/// [`ask`], with the wait for the answer, the control toggle timeout, that a test
/// shortens.
fn ask_within(dir: &Path, answer: Duration) -> Result<Listed, String> {
    let (mut client, member, before) = attach(dir)?;
    let own = client.client_id();
    let member_id = member.id();
    let toggle = match client.add_counter(
        CLUSTER_CONTROL_TOGGLE.0,
        &control_toggle_key(member_id, own),
        &format!("cluster.control_toggle {member_id}"),
    ) {
        Ok(toggle) => toggle,
        Err(error) => {
            let _ = client.close();
            return Err(format!(
                "the request for a snapshot could not be added: {error}"
            ));
        }
    };
    let answered = wait(&client, &member, &toggle, own, before, answer);
    // The toggle goes however the wait ended; the driver frees it too if this fails.
    toggle.close();
    let closed = client.close();
    let listed = answered?;
    closed.map_err(|error| {
        format!(
            "member {member_id} lists a snapshot of log position {}, and the command's \
             client did not close: {error}",
            listed.log_position
        )
    })?;
    Ok(listed)
}

/// Attaches to the driver, finds its member, and loads the id of the toggle the
/// member took last, which must be below this client's own: a member takes no
/// toggle at or below it. A later command may have been served between this
/// client's attach and its load, so the command attaches again for a higher id, a
/// few times.
fn attach(dir: &Path) -> Result<(Cavalry, Member, i64), String> {
    let mut last = None;
    for _ in 0..ATTACHES {
        let client = Cavalry::connect_to(dir).map_err(|error| error.to_string())?;
        let member = match Member::find(client.counters(), dir) {
            Ok(member) => member,
            Err(message) => {
                let _ = client.close();
                return Err(message);
            }
        };
        let taken = member.load(
            client.counters(),
            member.taken,
            CLUSTER_CONTROL_TOGGLE_TAKEN,
        );
        match taken {
            Some(taken) if taken < client.client_id() => return Ok((client, member, taken)),
            Some(taken) => last = Some((member.id(), taken, client.client_id())),
            None => {
                let message = member
                    .not_leading(client.counters(), When::Before)
                    .unwrap_or_else(|| gone(member.id(), When::Before));
                let _ = client.close();
                return Err(message);
            }
        }
        let _ = client.close();
    }
    let (member_id, taken, own) = last.unwrap_or((-1, -1, -1));
    Err(format!(
        "member {member_id}'s `cluster.control_toggle_taken` is {taken}, at or above this \
         command's client id, {own}, after {ATTACHES} attaches: a request was added with an \
         id that is no client's of this driver, and the member takes none below it. Nothing \
         was asked. Stop the member and start it again with `cavalry run`, which clears it, \
         then run `cavalry snapshot`; see {DOC}"
    ))
}

/// The message when the command's own client has closed under it: the driver went,
/// or timed this client out. `None` while the client is open.
fn driver_gone(client: &Cavalry, id: i32) -> Option<String> {
    client.closed().map(|closed| {
        format!(
            "the driver of member {id} went, or closed this command's client, while the \
             command waited ({closed:?}): what the member took is not known. Run `cavalry \
             doctor` on this host; when the driver and its member run again, run `cavalry \
             snapshot` on the host of the member that leads; see {DOC}"
        )
    })
}

/// Stores the request in `toggle` and waits, for `timeout` at the most, until the
/// member has taken a toggle at or above `own` and lists a snapshot at or past the
/// position it stored for it.
fn wait(
    client: &Cavalry,
    member: &Member,
    toggle: &Counter,
    own: i64,
    before: i64,
    timeout: Duration,
) -> Result<Listed, String> {
    let counters = client.counters();
    let id = member.id();
    toggle
        .store_release(CONTROL_TOGGLE_SNAPSHOT)
        .map_err(|error| format!("the request for a snapshot could not be stored: {error}"))?;
    let failed_before = member.load(counters, member.snapshots_failed, CLUSTER_SNAPSHOTS_FAILED);
    let deadline = Instant::now() + timeout;
    let seconds = timeout.as_secs_f64();
    // The member takes a toggle at or above this one: its own, or a later command's.
    // Either was taken after `before` was loaded, so its action was appended after
    // this command began.
    let position = loop {
        let taken = member.load(counters, member.taken, CLUSTER_CONTROL_TOGGLE_TAKEN);
        if taken.is_some_and(|taken| taken >= own) {
            // The id with an acquire load, then the position: the member stored the
            // position first (`spec/cluster.md`, "The control toggle").
            let position = member.load(
                counters,
                member.position,
                CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION,
            );
            if let Some(position) = position {
                break position;
            }
        }
        if let Some(message) = driver_gone(client, id) {
            return Err(message);
        }
        if let Some(message) = member.not_leading(counters, When::Waiting) {
            return Err(message);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "member {id} leads and did not take the request within {seconds} s: its \
                 `cluster.control_toggle_taken` is {}, as it was. The request was removed. A \
                 leader takes a request once it takes no snapshot of its own and its \
                 followers have committed the last request it made, so a snapshot of its own \
                 did not end, its followers do not answer, or its process has stopped and \
                 left its counters. `cavalry doctor` on this host says which, in its \
                 `cluster-member` note and its `cluster.` findings; then run `cavalry \
                 snapshot` again; see {DOC}",
                taken.unwrap_or(before)
            ));
        }
        std::thread::sleep(POLL);
    };
    loop {
        let listed = member.load(
            counters,
            member.snapshot_position,
            CLUSTER_SNAPSHOT_POSITION,
        );
        if let Some(log_position) = listed.filter(|listed| *listed >= position) {
            return Ok(Listed {
                member_id: id,
                asked_position: position,
                log_position,
            });
        }
        if let Some(message) = driver_gone(client, id) {
            return Err(message);
        }
        if let Some(message) = member.not_leading(counters, When::Taken(position)) {
            return Err(message);
        }
        if Instant::now() >= deadline {
            let failed = member.load(counters, member.snapshots_failed, CLUSTER_SNAPSHOTS_FAILED);
            let gave_up = match (failed_before, failed) {
                (Some(before), Some(now)) if now > before => {
                    "the member gave a snapshot up meanwhile, counted in \
                     `cluster.snapshots_failed`"
                }
                _ => "the member has not finished it",
            };
            return Err(format!(
                "member {id} asked every member for a snapshot of log position {position} and \
                 did not list its own within {seconds} s: its `cluster.snapshot_position` is \
                 {}, and {gave_up}. Nothing committed is lost. `cavalry doctor` on this host \
                 says why; then run `cavalry snapshot` again; see {DOC}",
                listed.unwrap_or(-1)
            ));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cavalry::Driver;
    use cavalry_proto::cluster::control_toggle_id;

    use super::*;

    /// A driver in this process, and a client that stands in for a cluster member:
    /// it adds the member's counters and stores what a test gives it. No consensus
    /// module runs: what is under test is what the command reads and says.
    struct Stand {
        dir: PathBuf,
        driver: Option<Driver>,
        member: Cavalry,
        counters: Vec<(CounterId, Counter)>,
    }

    const MEMBER: i32 = 1;

    /// The counters a member of this build adds, in the order it adds them here.
    const ALL: [CounterId; 7] = [
        CLUSTER_ROLE,
        CLUSTER_ELECTION_STATE,
        CLUSTER_LEADER_MEMBER_ID,
        CLUSTER_SNAPSHOT_POSITION,
        CLUSTER_SNAPSHOTS_FAILED,
        CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION,
        CLUSTER_CONTROL_TOGGLE_TAKEN,
    ];

    impl Stand {
        /// A driver with no member's counter.
        fn driver(test: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("cavalry-snapshot-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let driver =
                Driver::start(dir.join("driver"), dir.join("log")).expect("the driver starts");
            let member = Cavalry::connect_to(dir.join("driver")).expect("the member's client");
            Self {
                dir,
                driver: Some(driver),
                member,
                counters: Vec::new(),
            }
        }

        /// A member that leads with its election closed, has taken no toggle and lists
        /// no snapshot.
        fn leader(test: &str) -> Self {
            let mut stand = Self::driver(test);
            for type_id in ALL {
                stand.add(type_id, -1);
            }
            stand.set(CLUSTER_ROLE, LEADER);
            stand.set(CLUSTER_ELECTION_STATE, ELECTION_CLOSED);
            stand.set(CLUSTER_LEADER_MEMBER_ID, i64::from(MEMBER));
            stand.set(CLUSTER_SNAPSHOTS_FAILED, 0);
            stand
        }

        fn add(&mut self, type_id: CounterId, value: i64) {
            let counter = self
                .member
                .add_counter(type_id.0, &MEMBER.to_le_bytes(), "cluster 1")
                .expect("the member's counter");
            counter.store_release(value).expect("stored");
            self.counters.push((type_id, counter));
        }

        fn set(&self, type_id: CounterId, value: i64) {
            let (_, counter) = self
                .counters
                .iter()
                .find(|(id, _)| *id == type_id)
                .expect("the member has the counter");
            counter.store_release(value).expect("stored");
        }

        fn driver_dir(&self) -> PathBuf {
            self.dir.join("driver")
        }

        /// The command, on a thread of its own, with a wait of `answer` for its answer.
        fn ask(&self, answer: Duration) -> std::thread::JoinHandle<Result<Listed, String>> {
            let dir = self.driver_dir();
            std::thread::spawn(move || ask_within(&dir, answer))
        }

        /// The id of the control toggle beside the member that asks for a snapshot,
        /// once a command has added and stored one.
        fn toggle(&self) -> i64 {
            let counters = self.member.counters();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let found = (0..counters.capacity()).find_map(|id| {
                    let record = counters.record(id).ok().flatten()?;
                    (record.type_id() == CLUSTER_CONTROL_TOGGLE.0
                        && counters.read_acquire(id).ok() == Some(CONTROL_TOGGLE_SNAPSHOT))
                    .then(|| control_toggle_id(record.key(), MEMBER))
                    .flatten()
                });
                if let Some(toggle) = found {
                    return toggle;
                }
                assert!(Instant::now() < deadline, "the command added no toggle");
                std::thread::sleep(POLL);
            }
        }

        /// Whether any control toggle is in the counters file.
        fn has_toggle(&self) -> bool {
            find(self.member.counters(), CLUSTER_CONTROL_TOGGLE, &[]).is_some()
        }

        /// Waits until the driver has freed the command's toggle.
        fn toggle_gone(&self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.has_toggle() {
                assert!(Instant::now() < deadline, "the command left its toggle");
                std::thread::sleep(POLL);
            }
        }
    }

    impl Drop for Stand {
        fn drop(&mut self) {
            self.counters.clear();
            if let Some(driver) = self.driver.take() {
                let _ = driver.close();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn refused(asked: std::thread::JoinHandle<Result<Listed, String>>) -> String {
        let message = asked
            .join()
            .expect("the command's thread ended")
            .expect_err("the command is refused");
        assert!(message.ends_with(&format!("; see {DOC}")), "{message}");
        message
    }

    const SHORT: Duration = Duration::from_millis(200);
    const LONG: Duration = Duration::from_secs(20);

    /// A driver that hosts no member, and a member that does not lead: each is told
    /// before anything is asked, and no toggle is added.
    #[test]
    fn nothing_is_asked_of_no_member_or_one_that_does_not_lead() {
        let stand = Stand::driver("nobody");
        let message = refused(stand.ask(SHORT));
        assert!(
            message.starts_with("the driver in ")
                && message.contains("hosts no cluster member, so there is nobody to ask"),
            "{message}"
        );
        drop(stand);

        // A follower names its leader; a member in an election names none.
        let stand = Stand::leader("follower");
        stand.set(CLUSTER_ROLE, 0);
        stand.set(CLUSTER_LEADER_MEMBER_ID, 2);
        let message = refused(stand.ask(SHORT));
        assert!(
            message.starts_with(
                "member 1 does not lead, so nothing was asked: member 2 leads. Run `cavalry \
                 snapshot` on member 2's host, with its driver directory"
            ),
            "{message}"
        );
        stand.set(CLUSTER_ROLE, 1);
        stand.set(CLUSTER_ELECTION_STATE, 5);
        stand.set(CLUSTER_LEADER_MEMBER_ID, -1);
        let message = refused(stand.ask(SHORT));
        assert!(
            message.starts_with(
                "member 1 does not lead, so nothing was asked: the cluster is holding an \
                 election. Run `cavalry doctor` until"
            ),
            "{message}"
        );
        assert!(!stand.has_toggle());
    }

    /// A member that is starting has added some of its counters and not its gauges,
    /// and its election has not closed: it is told that it does not lead. A leader
    /// whose election has closed and that has no gauge is of a build before the
    /// control toggle, and is told so at once, with no wait for gauges that its build
    /// never adds (`spec/cluster.md`, "The control toggle"). No toggle is added.
    #[test]
    fn a_starting_member_does_not_lead_and_an_old_build_is_told_at_once() {
        // Its role alone, the first counter a member adds, and then its election state
        // and its leader as its start stores them: a follower, in `Init`, with none.
        let mut stand = Stand::driver("starting");
        stand.add(CLUSTER_ROLE, 0);
        let election = "member 1 does not lead, so nothing was asked: the cluster is holding \
                        an election. Run `cavalry doctor` until";
        let message = refused(stand.ask(LONG));
        assert!(message.starts_with(election), "{message}");
        stand.add(CLUSTER_ELECTION_STATE, 0);
        stand.add(CLUSTER_LEADER_MEMBER_ID, -1);
        let message = refused(stand.ask(LONG));
        assert!(message.starts_with(election), "{message}");
        assert!(!stand.has_toggle());
        drop(stand);

        // A member of a build before the control toggle: every counter of this build
        // but the two gauges, leading with its election closed.
        let mut stand = Stand::driver("old");
        for type_id in [
            CLUSTER_ROLE,
            CLUSTER_ELECTION_STATE,
            CLUSTER_LEADER_MEMBER_ID,
            CLUSTER_SNAPSHOT_POSITION,
            CLUSTER_SNAPSHOTS_FAILED,
        ] {
            stand.add(type_id, -1);
        }
        stand.set(CLUSTER_ROLE, LEADER);
        stand.set(CLUSTER_ELECTION_STATE, ELECTION_CLOSED);
        stand.set(CLUSTER_LEADER_MEMBER_ID, i64::from(MEMBER));
        stand.set(CLUSTER_SNAPSHOTS_FAILED, 0);
        let began = Instant::now();
        let message = refused(stand.ask(LONG));
        let took = began.elapsed();
        assert!(
            message.starts_with(
                "member 1 is of a build that takes no request for a snapshot: it leads, with \
                 its election closed, and has no `cluster.control_toggle_taken` counter. \
                 Nothing was asked. Upgrade the member"
            ),
            "{message}"
        );
        // An attach and one look, with no wait for the gauges, so that an upgrade one
        // member at a time is not held for the command (ADR 0147, Consequences).
        assert!(took < Duration::from_millis(500), "{took:?}");
        assert!(!stand.has_toggle());
    }

    /// The command is answered when the member has taken a toggle at or above its own
    /// and lists a snapshot at or past the position it stored: its own toggle, a
    /// later command's, and a later snapshot than the one asked for, which the
    /// answer tells apart. A snapshot the member listed before does not answer it.
    /// The toggle is gone when the command has ended.
    #[test]
    fn the_command_is_answered_by_a_toggle_at_or_above_its_own_and_a_snapshot_at_its_position() {
        let stand = Stand::leader("answered");
        // A snapshot listed before the command began, at 96.
        stand.set(CLUSTER_SNAPSHOT_POSITION, 96);
        let asked = stand.ask(LONG);
        let toggle = stand.toggle();
        // Taken, and not yet listed: the command waits.
        stand.set(CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION, 288);
        stand.set(CLUSTER_CONTROL_TOGGLE_TAKEN, toggle);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!asked.is_finished(), "a snapshot of 96 answers no request");
        stand.set(CLUSTER_SNAPSHOT_POSITION, 288);
        assert_eq!(
            asked.join().expect("the thread ended"),
            Ok(Listed {
                member_id: MEMBER,
                asked_position: 288,
                log_position: 288
            })
        );
        stand.toggle_gone();

        // A later command's toggle answers this one, and a later snapshot than the one
        // asked for is told as one.
        let asked = stand.ask(LONG);
        let toggle = stand.toggle();
        assert!(toggle > 0);
        stand.set(CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION, 384);
        stand.set(CLUSTER_CONTROL_TOGGLE_TAKEN, toggle + 7);
        stand.set(CLUSTER_SNAPSHOT_POSITION, 480);
        assert_eq!(
            asked.join().expect("the thread ended"),
            Ok(Listed {
                member_id: MEMBER,
                asked_position: 384,
                log_position: 480
            })
        );
        stand.toggle_gone();
    }

    /// A leader that takes no toggle, and one that takes it and lists no snapshot:
    /// the command ends at its timeout, says which, and removes its toggle. A
    /// snapshot the member gave up meanwhile is named.
    #[test]
    fn the_command_ends_at_its_timeout_and_says_how_far_the_member_came() {
        let stand = Stand::leader("timeout");
        let asked = stand.ask(SHORT);
        stand.toggle();
        let message = refused(asked);
        assert!(
            message.starts_with(
                "member 1 leads and did not take the request within 0.2 s: its \
                 `cluster.control_toggle_taken` is -1, as it was. The request was removed."
            ),
            "{message}"
        );
        stand.toggle_gone();

        let asked = stand.ask(SHORT);
        let toggle = stand.toggle();
        stand.set(CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION, 192);
        stand.set(CLUSTER_CONTROL_TOGGLE_TAKEN, toggle);
        stand.set(CLUSTER_SNAPSHOTS_FAILED, 1);
        let message = refused(asked);
        assert!(
            message.starts_with(
                "member 1 asked every member for a snapshot of log position 192 and did not \
                 list its own within 0.2 s: its `cluster.snapshot_position` is -1, and the \
                 member gave a snapshot up meanwhile, counted in `cluster.snapshots_failed`. \
                 Nothing committed is lost."
            ),
            "{message}"
        );
        stand.toggle_gone();
    }

    /// A member that stops leading while the command waits ends the wait at once,
    /// before the toggle is taken and after, each with what became of the request
    /// and where to ask.
    #[test]
    fn a_member_that_stops_leading_ends_the_wait_at_once() {
        let stand = Stand::leader("deposed");
        let asked = stand.ask(LONG);
        stand.toggle();
        stand.set(CLUSTER_LEADER_MEMBER_ID, 0);
        stand.set(CLUSTER_ROLE, 0);
        let message = refused(asked);
        assert!(
            message.starts_with(
                "member 1 stopped leading while the request waited, and the request was \
                 removed: member 0 leads. Run `cavalry snapshot` on member 0's host"
            ),
            "{message}"
        );
        stand.toggle_gone();

        stand.set(CLUSTER_LEADER_MEMBER_ID, i64::from(MEMBER));
        stand.set(CLUSTER_ROLE, LEADER);
        let asked = stand.ask(LONG);
        let toggle = stand.toggle();
        stand.set(CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION, 192);
        stand.set(CLUSTER_CONTROL_TOGGLE_TAKEN, toggle);
        std::thread::sleep(Duration::from_millis(20));
        stand.set(CLUSTER_ELECTION_STATE, 5);
        stand.set(CLUSTER_ROLE, 1);
        let message = refused(asked);
        assert!(
            message.starts_with(
                "member 1 took the request, asked every member for a snapshot of log \
                 position 192, and stopped leading before it listed its own."
            ) && message.contains("the cluster is holding an election"),
            "{message}"
        );
        stand.toggle_gone();
    }

    /// A member whose last toggle has an id no client of the driver has yet: the
    /// command attaches three times for a higher id, asks nothing, and says how to
    /// clear it.
    #[test]
    fn a_toggle_id_that_is_no_clients_is_told_after_three_attaches() {
        let stand = Stand::leader("poisoned");
        stand.set(CLUSTER_CONTROL_TOGGLE_TAKEN, i64::MAX);
        let message = refused(stand.ask(SHORT));
        assert!(
            message.starts_with(&format!(
                "member 1's `cluster.control_toggle_taken` is {}, at or above this command's \
                 client id, ",
                i64::MAX
            )) && message.contains("after 3 attaches")
                && message.contains("Stop the member and start it again with `cavalry run`"),
            "{message}"
        );
        assert!(!stand.has_toggle());
    }
}
