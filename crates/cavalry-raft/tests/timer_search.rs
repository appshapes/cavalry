//! A search for a timer handed to a program twice, early, after its cancel or not at
//! all (`spec/cluster.md`, "The service", "Timers"; ADR 0151; CLAUDE.md, invariant 1).
//! Three members in memory under a delivery seed of loss and delay, with members
//! halted and resumed, cut from each other and started again at moments the seed
//! draws, and beside each member a service as the page gives it: the table of
//! pending timers that a program schedules and cancels in, what the service tells
//! its member, and the member's own timers, which append a timer event when the
//! member leads and its clock has reached a deadline. One member of the three, which
//! the seed draws, has a second service beside it, which reads at its own pace and
//! tells the same member, and may stop for good with the member told what that
//! service last knew.
//!
//! What the seed draws against the rule: a service that reads its log late, so that
//! its leader appends events for timers that are cancelled or scheduled again
//! already; a handler that does not take its entry; a consensus module stream that
//! takes no message for a while; a member's consensus module that stands still while
//! its driver and its services run on, the leader's half the time (`Cluster::stall`;
//! work item 467); a service that stops, and starts again from the
//! snapshot it took or from position 0, beside a member that kept what the service
//! before it had told it, and reads its history before it tells that member
//! anything, in half of the seeds, and in the other half no service that stops, so
//! that nothing but the rule puts right what a member lost; a member that starts
//! again and holds no timer; members whose wall
//! clocks are apart, so that a new leader's clock is behind the last one's; and a
//! leader that was cut off and appends events to a log no other member holds.
//!
//! The check of every step is `elected_cluster`'s, and with it:
//!
//! - **One answer at one position.** A program's state after an entry is one value
//!   beside every member, whichever service reaches the position first, and again
//!   for a service that reads the position a second time after a start.
//! - **The rule, by a second reading of it.** Every timer event of the committed log
//!   is handed over or passed over as a replay of the log by a few lines of this
//!   file says, which holds a map of ids to deadlines and nothing of
//!   [`ServiceTimers`]: handed when the timer is pending with a deadline at or before
//!   the event's time, and then pending no more. So no timer is handed over twice,
//!   early, or after its cancel.
//! - **Every timer is handed over.** At the end of a seed the cluster is left to
//!   settle for [`SETTLE_MS`] with nothing cut, lost or late and every member and
//!   service running, and then no service holds a timer whose deadline the leader's
//!   clock passed in the first half of that time, and the leader's member holds no
//!   such timer that it has appended no event for. A member
//!   whose election has not closed by then is left out of this check and counted,
//!   which the test prints with its seed: that is a member the cluster has not taken
//!   back, work item 420, and no timer's doing. A seed that ends with two such
//!   members has no quorum to hand a timer over: its last checks are not made, it
//!   is counted, and more than one such seed in twenty fails the test.
//!
//! Each run of `just test` searches the seeds of [`SEEDS`]. A wider search names its
//! own: `TIMER_SEARCH_FROM`, `TIMER_SEARCH_COUNT` and `TIMER_SEARCH_MS`. The test's
//! last asserts are over all the seeds of a run: that they reach what the search is
//! for, a service started again among it, and that no more than one in twenty ends
//! with no settled quorum. A run of one seed may fail them with no check failed, so
//! read such a run by the lines it prints, the seed's own and the run's summary.

mod elected_cluster;

use std::collections::{BTreeMap, HashMap, VecDeque};

use cavalry_proto::cluster::{Message, ServiceTimers, Stream, TimerEventIs, TimerWord};
use cavalry_raft::memory::Cluster;
use cavalry_raft::{ElectionState, MemberTimers, Role};
use elected_cluster::Safety;

/// The members.
const IDS: [i32; 3] = [0, 1, 2];

/// The seeds `just test` runs.
const SEEDS: [u64; 4] = [1, 2, 3, 13];

/// How long a seed's cluster is left to settle before the last check: longer than the
/// startup canvass timeout, 60 s, which a member started just before may canvass for.
/// A timer whose deadline lies in the first half of it is owed by its end.
const SETTLE_MS: u64 = 100_000;

/// The timer refresh interval, in steps of a millisecond (`spec/cluster.md`,
/// "Values").
const REFRESH_MS: u64 = cavalry_proto::cluster::TIMER_REFRESH_INTERVAL_NS / 1_000_000;

/// More timer events than any seed appends whose members mark what they appended:
/// a seed of a minute appends a few hundred.
const MOST_EVENTS: u64 = 100_000;

/// The correlation ids the program schedules: few, so that a timer is scheduled
/// again, cancelled and due again many times in one seed.
const TIMER_IDS: i64 = 12;

/// A timer whose id is a multiple of this repeats: its handler schedules it again.
const REPEATS: i64 = 4;

/// How long after its event a timer that repeats is due again.
const PERIOD_MS: i64 = 700;

/// A xorshift generator: the search is the seed's alone.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        let len = u64::try_from(items.len()).expect("a short list");
        let index = usize::try_from(self.below(len)).expect("an index of the list");
        items[index]
    }
}

fn setting(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// What a session's message asks the program to do: the payload of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Schedule { id: i64, deadline: i64 },
    Cancel { id: i64 },
    Snapshot,
}

impl Command {
    fn payload(self) -> Vec<u8> {
        let (op, id, deadline) = match self {
            Command::Schedule { id, deadline } => (1u8, id, deadline),
            Command::Cancel { id } => (2, id, 0),
            Command::Snapshot => (3, 0, 0),
        };
        let mut bytes = vec![op];
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&deadline.to_le_bytes());
        bytes
    }

    fn read(payload: &[u8]) -> Option<Command> {
        let id = i64::from_le_bytes(payload.get(1..9)?.try_into().ok()?);
        let deadline = i64::from_le_bytes(payload.get(9..17)?.try_into().ok()?);
        match payload.first()? {
            1 => Some(Command::Schedule { id, deadline }),
            2 => Some(Command::Cancel { id }),
            3 => Some(Command::Snapshot),
            _ => None,
        }
    }
}

/// One step of the program's state: a hash over everything the program was handed
/// and what it was answered, so that two states are equal only after the same.
fn mix(state: u64, values: &[i64]) -> u64 {
    let mut hash = state ^ 0xcbf2_9ce4_8422_2325;
    for value in values {
        for byte in value.to_le_bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// What a snapshot of a service holds: where its read begins, its timers, and its
/// program's state.
#[derive(Clone)]
struct Snapshot {
    position: i64,
    timers: Vec<(i64, i64)>,
    state: u64,
}

/// A service beside one member: its program's state, its timers, and where it has
/// read its member's committed log to.
struct Service {
    timers: Box<ServiceTimers>,
    /// The history it reads at its start, by the rule of "Replies": its member's
    /// term and append position when the service opened the log. An entry of a term
    /// below that one, or of that term and below that position, is history. `None`
    /// once it has read past it and told its member its timers.
    history_until: Option<(i64, i64)>,
    /// The step at which it last sent every pending timer again.
    refreshed_at: u64,
    position: i64,
    state: u64,
    passed_over: u64,
}

impl Service {
    fn start(snapshot: Option<&Snapshot>, member_is_at: (i64, i64)) -> Self {
        let mut timers = Box::new(ServiceTimers::new());
        timers.hold();
        let (position, state) = match snapshot {
            Some(snapshot) => {
                for &(id, deadline) in &snapshot.timers {
                    timers
                        .load(id, deadline)
                        .expect("a snapshot's timers in order");
                }
                (snapshot.position, snapshot.state)
            }
            None => (0, 0),
        };
        Service {
            timers,
            history_until: Some(member_is_at),
            refreshed_at: 0,
            position,
            state,
            passed_over: 0,
        }
    }
}

/// The rule read a second time: the committed log replayed over a map of ids to
/// deadlines, which says for each timer event whether it is handed over, and the
/// program's state after each entry, for every service to be held to.
#[derive(Default)]
struct Canon {
    /// Where the replay has read to.
    position: i64,
    pending: HashMap<i64, i64>,
    state: u64,
    /// For each entry, by the position after it: its bytes, whether it is a timer
    /// event that is handed over, and the program's state after it.
    entries: BTreeMap<i64, (Vec<u8>, bool, u64)>,
    events: u64,
    handed: u64,
}

impl Canon {
    /// Replays one committed entry, which begins where the replay has read to.
    fn read(&mut self, position: i64, after: i64, bytes: &[u8]) {
        assert_eq!(position, self.position, "the canon reads the log in order");
        let mut handed = false;
        match Message::parse(Stream::Log, bytes).expect("an entry of the log") {
            Message::SessionMessage { payload, .. } => match Command::read(payload) {
                Some(Command::Schedule { id, deadline }) => {
                    self.pending.insert(id, deadline);
                    self.state = mix(self.state, &[1, position, id, deadline, 0]);
                }
                Some(Command::Cancel { id }) => {
                    self.pending.remove(&id);
                    self.state = mix(self.state, &[2, position, id]);
                }
                Some(Command::Snapshot) | None => {
                    self.state = mix(self.state, &[3, position]);
                }
            },
            Message::TimerEvent {
                correlation_id,
                timestamp,
                ..
            } => {
                self.events += 1;
                handed = self
                    .pending
                    .get(&correlation_id)
                    .is_some_and(|deadline| *deadline <= timestamp);
                if handed {
                    self.handed += 1;
                    self.pending.remove(&correlation_id);
                    if correlation_id % REPEATS == 0 {
                        self.pending.insert(correlation_id, timestamp + PERIOD_MS);
                    }
                    self.state = mix(self.state, &[4, position, correlation_id, timestamp]);
                }
            }
            _ => {}
        }
        self.position = after;
        self.entries
            .insert(after, (bytes.to_vec(), handed, self.state));
    }
}

/// What one seed's run drew and found.
#[derive(Default)]
struct Drawn {
    events: u64,
    handed: u64,
    passed_over: u64,
    not_taken: u64,
    service_starts: u64,
    member_starts: u64,
    stale_leader_events: u64,
    stalls: u64,
    leaders_stalled: u64,
    unsettled_members: u64,
    unsettled_clusters: u64,
    leader_cuts: u64,
    refreshes: u64,
}

/// One member's side of the model beside the cluster's own.
struct Beside {
    /// The services beside the member: one, or two for the member the seed drew.
    /// The first is the one that takes snapshots.
    services: Vec<Option<Service>>,
    snapshot: Option<Snapshot>,
    member: MemberTimers,
    /// The consensus module stream: what the services' publications hold that the
    /// member has not read.
    stream: VecDeque<TimerWord>,
    /// How far the member's wall clock is from the cluster's.
    skew_ms: i64,
    /// Until when each of its services reads nothing, in steps.
    service_idle_until: [u64; 2],
    /// Until when the stream takes nothing.
    stream_full_until: u64,
}

/// A member's term and its append position, as a service loads them when it opens
/// the member's log.
fn member_is_at(cluster: &Cluster, member: i32) -> (i64, i64) {
    (
        cluster
            .member(member)
            .map_or(0, |core| core.leadership_term_id()),
        cluster.append_position(member).unwrap_or(0),
    )
}

/// The service at `index` beside `member` reads a few committed entries, and tells
/// its member what the stream takes.
#[allow(clippy::too_many_arguments)]
fn service_turn(
    cluster: &Cluster,
    member: i32,
    beside: &mut Beside,
    index: usize,
    canon: &mut Canon,
    draw: &mut Draw,
    step: u64,
    drawn: &mut Drawn,
) {
    // A service whose program does not read does nothing: it reads no entry, sends
    // nothing and refreshes nothing.
    if beside
        .service_idle_until
        .get(index)
        .is_some_and(|until| step < *until)
    {
        return;
    }
    let Some(service) = beside.services.get_mut(index).and_then(Option::as_mut) else {
        return;
    };
    let commit = cluster.commit_position(member).unwrap_or(0);
    // The end of history when the service has read to where its member's log ended
    // when it opened it: the member is told every pending timer.
    if service
        .history_until
        .is_some_and(|(_, until)| service.position >= until)
    {
        service.history_until = None;
        service.timers.release();
        service.refreshed_at = step;
    }
    // And again at each timer refresh interval after.
    if service.history_until.is_none() && step >= service.refreshed_at + REFRESH_MS {
        service.timers.tell_all_again();
        service.refreshed_at = step;
        drawn.refreshes += 1;
    }
    {
        let limit = usize::try_from(1 + draw.below(8)).expect("a few");
        for (position, after, bytes) in cluster
            .entries(member, service.position, commit)
            .into_iter()
            .take(limit)
        {
            assert_eq!(
                position, service.position,
                "member {member}'s log has a gap"
            );
            if after > canon.position {
                canon.read(position, after, &bytes);
            }
            let (canon_bytes, canon_handed, canon_state) =
                canon.entries.get(&after).expect("the canon has read it");
            assert_eq!(
                &bytes, canon_bytes,
                "member {member} holds other committed bytes at {position}"
            );
            let message = Message::parse(Stream::Log, &bytes).expect("an entry of the log");
            // The end of history at the first entry that is none: one of a later
            // term than the member was in, as every entry is that follows a cut of
            // the member's log below where it ended.
            let term = match message {
                Message::SessionMessage {
                    leadership_term_id, ..
                }
                | Message::TimerEvent {
                    leadership_term_id, ..
                }
                | Message::NewLeadershipTermEvent {
                    leadership_term_id, ..
                } => Some(leadership_term_id),
                _ => None,
            };
            if let (Some((history_term, until)), Some(term)) = (service.history_until, term)
                && (term > history_term || (term == history_term && position >= until))
            {
                service.history_until = None;
                service.timers.release();
                service.refreshed_at = step;
            }
            // One handler in sixteen does not take its entry, and is handed it again.
            let mut taken = draw.below(16) != 0;
            match message {
                Message::SessionMessage { payload, .. } => {
                    service.timers.begin_entry();
                    let command = Command::read(payload);
                    match command {
                        Some(Command::Schedule { id, deadline }) => {
                            let refused = service.timers.schedule(id, deadline).is_err();
                            assert!(!refused, "twelve ids are never at the bound");
                        }
                        Some(Command::Cancel { id }) => service.timers.cancel(id),
                        Some(Command::Snapshot) | None => {}
                    }
                    service.timers.end_entry(taken);
                    if taken {
                        service.state = match command {
                            Some(Command::Schedule { id, deadline }) => {
                                mix(service.state, &[1, position, id, deadline, 0])
                            }
                            Some(Command::Cancel { id }) => mix(service.state, &[2, position, id]),
                            Some(Command::Snapshot) | None => mix(service.state, &[3, position]),
                        };
                        if command == Some(Command::Snapshot) && index == 0 {
                            beside.snapshot = Some(Snapshot {
                                position: after,
                                timers: service.timers.iter().collect(),
                                state: service.state,
                            });
                        }
                    }
                }
                Message::TimerEvent {
                    correlation_id,
                    timestamp,
                    ..
                } => match service.timers.begin_event(correlation_id, timestamp) {
                    TimerEventIs::Handed => {
                        assert!(
                            *canon_handed,
                            "member {member}'s service hands over the event of timer \
                             {correlation_id} at {position}, which the rule passes over"
                        );
                        if correlation_id % REPEATS == 0 {
                            service
                                .timers
                                .schedule(correlation_id, timestamp + PERIOD_MS)
                                .expect("a pending id is scheduled at any count");
                        }
                        service.timers.end_entry(taken);
                        if taken {
                            service.state =
                                mix(service.state, &[4, position, correlation_id, timestamp]);
                        }
                    }
                    TimerEventIs::NotPending | TimerEventIs::Early(_) => {
                        assert!(
                            !*canon_handed,
                            "member {member}'s service passes over the event of timer \
                             {correlation_id} at {position}, which the rule hands over"
                        );
                        // A passed-over event is handed to no handler, so it is taken.
                        taken = true;
                        service.timers.end_entry(true);
                        service.passed_over += 1;
                    }
                },
                _ => {}
            }
            if !taken {
                drawn.not_taken += 1;
                break;
            }
            service.position = after;
            assert_eq!(
                service.state, *canon_state,
                "member {member}'s program is in another state after the entry at {position} \
                 than the first program to read it"
            );
        }
    }
    // What the service tells its member, as the stream takes it.
    while step >= beside.stream_full_until && beside.stream.len() < 32 {
        let Some(word) = service.timers.next_word() else {
            break;
        };
        beside.stream.push_back(word);
        service.timers.told(word);
    }
}

/// The member reads its consensus module stream, and appends the timer events that
/// are due while it leads.
fn member_turn(
    cluster: &mut Cluster,
    member: i32,
    beside: &mut Beside,
    cut_off_now: bool,
    drawn: &mut Drawn,
) {
    for _ in 0..10 {
        match beside.stream.pop_front() {
            Some(TimerWord::Schedule {
                correlation_id,
                deadline,
            }) => {
                beside.member.schedule(correlation_id, deadline);
            }
            Some(TimerWord::Cancel { correlation_id }) => {
                beside.member.cancel(correlation_id);
            }
            None => break,
        }
    }
    let leads = cluster.role(member) == Some(Role::Leader)
        && cluster.election_state(member) == Some(ElectionState::Closed);
    if !leads {
        return;
    }
    let now = cluster.time_ms() + beside.skew_ms;
    for _ in 0..10 {
        let Some(id) = beside.member.due(now) else {
            break;
        };
        match cluster.timer_event(member, id, beside.skew_ms) {
            Ok(position) => {
                beside.member.appended(id, position);
                drawn.events += 1;
                assert!(
                    drawn.events < MOST_EVENTS,
                    "{MOST_EVENTS} timer events appended: the log does not come to rest"
                );
                if cluster.leader() != Some(member) || cut_off_now {
                    drawn.stale_leader_events += 1;
                }
            }
            Err(_) => break,
        }
    }
}

/// Runs `seed` for `length_ms` and then lets it settle, the checks of every step
/// failing the test where they fail, and returns what it drew.
fn search(seed: u64, length_ms: u64) -> Drawn {
    let mut safety = Safety::new(&IDS);
    let mut cluster = Cluster::with_election(&IDS, seed).expect("three members");
    let mut draw = Draw(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let delay = draw.pick(&[0, 0, 20, 200, 2_000]);
    let loss = draw.pick(&[0, 0, 200, 30]);
    cluster.set_delivery(loss, delay);
    let mut canon = Canon::default();
    let mut drawn = Drawn::default();
    // How often a service stops or starts: in some seeds never, so that nothing but
    // the rule puts right what a member lost.
    let service_stops = draw.pick(&[0, 0, 1, 3]);
    // One member has two services beside it, which read at their own pace.
    let two = draw.pick(&IDS);
    let mut besides: Vec<Beside> = IDS
        .iter()
        .map(|&member| Beside {
            services: (0..if member == two { 2 } else { 1 })
                .map(|_| Some(Service::start(None, (0, 0))))
                .collect(),
            snapshot: None,
            member: MemberTimers::new(),
            stream: VecDeque::new(),
            skew_ms: i64::try_from(draw.below(1_200)).expect("a skew") - 600,
            service_idle_until: [0; 2],
            stream_full_until: 0,
        })
        .collect();
    let mut halted: Option<(i32, u64)> = None;
    let mut cut: Option<(i32, i32, u64)> = None;
    let mut cut_off: Option<(i32, u64)> = None;
    let mut stalled: Option<(i32, u64)> = None;
    let total = length_ms + SETTLE_MS;
    for ms in 0..total {
        let searching = ms < length_ms;
        if let Some((member, until)) = halted
            && (ms >= until || !searching)
        {
            cluster.resume(member);
            halted = None;
        }
        if let Some((a, b, until)) = cut
            && (ms >= until || !searching)
        {
            cluster.heal_between(a, b);
            cut = None;
        }
        if let Some((member, until)) = cut_off
            && (ms >= until || !searching)
        {
            cluster.heal(member);
            cut_off = None;
        }
        if let Some((member, until)) = stalled
            && (ms >= until || !searching)
        {
            cluster.unstall(member);
            stalled = None;
        }
        if !searching {
            // Settling: nothing is lost or late, and every service runs, reads and tells.
            if ms == length_ms {
                cluster.set_delivery(0, 0);
            }
            for (&member, beside) in IDS.iter().zip(&mut besides) {
                // A member's first service runs; a second one that stopped stays
                // stopped, and what it last told the member stays told.
                if ms == length_ms && beside.services[0].is_none() {
                    let appended = member_is_at(&cluster, member);
                    beside.services[0] = Some(Service::start(beside.snapshot.as_ref(), appended));
                    drawn.service_starts += 1;
                }
                beside.service_idle_until = [0; 2];
                beside.stream_full_until = 0;
            }
        }
        if searching {
            // What sessions ask of the program.
            if draw.below(12) == 0 {
                let id = i64::try_from(draw.below(u64::try_from(TIMER_IDS).expect("ids")))
                    .expect("an id");
                let command = match draw.below(10) {
                    0..=5 => Command::Schedule {
                        id,
                        deadline: cluster.time_ms()
                            + i64::from(draw.pick(&[0, 5, 50, 400, 3_000, 20_000]))
                            - 20,
                    },
                    6..=8 => Command::Cancel { id },
                    _ => Command::Snapshot,
                };
                let _ = cluster.offer(&command.payload());
            }
            match draw.below(2_500) {
                0 if halted.is_none() => {
                    let member = draw.pick(&IDS);
                    if stalled.is_none_or(|(stalled, _)| stalled != member) {
                        cluster.halt(member);
                        halted = Some((member, ms + 300 + draw.below(15_000)));
                    }
                }
                1 if cut.is_none() => {
                    let a = draw.pick(&IDS);
                    let b = draw.pick(&[(a + 1) % 3, (a + 2) % 3]);
                    if draw.below(2) == 0 {
                        cluster.cut_one_way(a, b);
                    } else {
                        cluster.cut_between(a, b);
                    }
                    cut = Some((a, b, ms + 200 + draw.below(12_000)));
                }
                2 => {
                    // A member starts again on the log directory it kept: it holds no
                    // timer, what its stream held unread is gone with it, and its
                    // services read that it began again.
                    let member = draw.pick(&IDS);
                    if halted.is_none_or(|(halted, _)| halted != member) {
                        safety.restart(&mut cluster, member);
                        // A start ends a stall: the process that stood still is gone.
                        if stalled.is_some_and(|(stalled, _)| stalled == member) {
                            stalled = None;
                        }
                        let beside = &mut besides[usize::try_from(member).expect("an id")];
                        beside.member.clear();
                        beside.stream.clear();
                        drawn.member_starts += 1;
                    }
                }
                at @ 3..=5 if at - 3 < service_stops => {
                    // A service stops, or one that had stopped starts: from its
                    // snapshot, or from position 0 when the seed says so.
                    let member = draw.pick(&IDS);
                    let appended = member_is_at(&cluster, member);
                    let beside = &mut besides[usize::try_from(member).expect("an id")];
                    let which =
                        usize::try_from(draw.below(2)).expect("a service") % beside.services.len();
                    if beside.services[which].is_some() {
                        beside.services[which] = None;
                    } else {
                        let from = if draw.below(4) == 0 {
                            None
                        } else {
                            beside.snapshot.as_ref()
                        };
                        beside.services[which] = Some(Service::start(from, appended));
                        drawn.service_starts += 1;
                    }
                }
                6..=9 => {
                    let beside = &mut besides[usize::try_from(draw.pick(&IDS)).expect("an id")];
                    let which = usize::try_from(draw.below(2)).expect("a service");
                    beside.service_idle_until[which] = ms + draw.pick(&[5, 50, 500, 4_000]);
                }
                10..=12 => {
                    let beside = &mut besides[usize::try_from(draw.pick(&IDS)).expect("an id")];
                    beside.stream_full_until = ms + draw.pick(&[5, 50, 500, 4_000]);
                }
                13 | 14 if cut_off.is_none() && cut.is_none() => {
                    // The leader is cut off from both others: it leads on, and appends
                    // events to a log that no other member holds, until it has heard
                    // no quorum for the leader heartbeat timeout.
                    if let Some(leader) = cluster.leader() {
                        drawn.leader_cuts += 1;
                        cluster.cut(leader);
                        cut_off = Some((leader, ms + 9_000 + draw.below(20_000)));
                    }
                }
                // A module that stands still while its driver runs on (work item 467):
                // the leader's half the time, so that it stalls with entries appended
                // after its duty cycle read its append position. Its recording syncs, its
                // replays and its sender go on, its services run, and what is sent to it
                // and what its services tell it wait, for 1 ms to 20 s: within a
                // heartbeat interval, or long enough that the others elect without it.
                15 if stalled.is_none() => {
                    let member = match cluster.leader() {
                        Some(leader) if draw.below(2) == 0 => leader,
                        _ => draw.pick(&IDS),
                    };
                    if halted.is_none_or(|(halted, _)| halted != member) {
                        let span = draw.pick(&[2, 20, 200, 2_000, 20_000]);
                        if cluster.leader() == Some(member) {
                            drawn.leaders_stalled += 1;
                        }
                        cluster.stall(member);
                        stalled = Some((member, ms + 1 + draw.below(span)));
                        drawn.stalls += 1;
                    }
                }
                _ => {}
            }
        }
        safety.run(&mut cluster, 1);
        for (&member, beside) in IDS.iter().zip(&mut besides) {
            if halted.is_some_and(|(halted, _)| halted == member) {
                continue;
            }
            for index in 0..beside.services.len() {
                service_turn(
                    &cluster, member, beside, index, &mut canon, &mut draw, ms, &mut drawn,
                );
            }
            // A stalled module reads nothing its services told it and appends no
            // timer event; its services run on.
            if stalled.is_some_and(|(stalled, _)| stalled == member) {
                continue;
            }
            let cut_off_now = cut_off.is_some_and(|(cut, _)| cut == member);
            member_turn(&mut cluster, member, beside, cut_off_now, &mut drawn);
        }
    }
    // A timer that repeats is due again each period, so the log grows while the
    // cluster settles: the last check waits for a step at which every member holds
    // one log, every service has read all of it and has told its member everything,
    // and every member has read what it was told.
    //
    // A member whose election is not closed by now is left out, and counted: that is
    // the cluster's to explain and no timer's (work item 420). Seeds of earlier forms
    // of this search left one out, a member that asked its leader for a catch-up every
    // ten seconds and was sent none, with nothing cut and nothing lost (ADR 0151,
    // "Evidence"); seed 101 was one of them, and at this form it leaves none. Seed 23
    // leaves two, beside a leader whose log no quorum holds: a cluster that commits
    // nothing hands no timer over, so none of the seed's last checks is made, and it
    // is counted.
    let settled = |cluster: &Cluster, member: i32| {
        cluster.election_state(member) == Some(ElectionState::Closed)
    };
    let unsettled = IDS
        .iter()
        .filter(|&&member| !settled(&cluster, member))
        .count();
    drawn.unsettled_members += u64::try_from(unsettled).expect("a count");
    drawn.handed = canon.handed;
    if unsettled > 1 {
        drawn.unsettled_clusters += 1;
        drawn.passed_over = besides
            .iter()
            .flat_map(|beside| beside.services.iter().flatten())
            .map(|service| service.passed_over)
            .sum();
        return drawn;
    }
    let mut ms = total;
    while IDS.iter().zip(&besides).any(|(&member, beside)| {
        settled(&cluster, member)
            && (cluster.log_end(member) != canon.position
                || !beside.stream.is_empty()
                || beside.services[0].is_none()
                || beside.services.iter().flatten().any(|service| {
                    service.position != canon.position
                        || service.history_until.is_some()
                        || service.timers.next_word().is_some()
                }))
    }) {
        assert!(
            ms < total + 10_000,
            "seed {seed}: the members and their services did not reach one position, with \
             nothing left to tell, in 10 s: {:?}",
            IDS.iter()
                .zip(&besides)
                .map(|(&member, beside)| (
                    member,
                    settled(&cluster, member),
                    cluster.log_end(member),
                    canon.position,
                    beside.stream.iter().copied().collect::<Vec<_>>(),
                    beside
                        .services
                        .iter()
                        .map(|service| service.as_ref().map(|service| (
                            service.position,
                            service.history_until,
                            service.timers.next_word()
                        )))
                        .collect::<Vec<_>>(),
                    beside.member.iter().collect::<Vec<_>>(),
                ))
                .collect::<Vec<_>>()
        );
        safety.run(&mut cluster, 1);
        for (&member, beside) in IDS.iter().zip(&mut besides) {
            for index in 0..beside.services.len() {
                service_turn(
                    &cluster, member, beside, index, &mut canon, &mut draw, ms, &mut drawn,
                );
            }
            member_turn(&mut cluster, member, beside, false, &mut drawn);
        }
        ms += 1;
    }
    // Every service has read the whole committed log to one state, and no timer that
    // was due before the cluster settled is pending still.
    let leader = cluster
        .leader()
        .unwrap_or_else(|| panic!("seed {seed}: no member leads after {SETTLE_MS} ms of settling"));
    let leader_now = cluster.time_ms() + besides[usize::try_from(leader).expect("an id")].skew_ms;
    let owed_before = leader_now - i64::try_from(SETTLE_MS / 2).expect("a time");
    for (&member, beside) in IDS.iter().zip(&besides) {
        if !settled(&cluster, member) {
            continue;
        }
        assert!(beside.services[0].is_some(), "a service runs at the end");
        for service in beside.services.iter().flatten() {
            assert_eq!(
                service.position, canon.position,
                "seed {seed}: member {member}'s service has not read the committed log to its end"
            );
            assert_eq!(service.state, canon.state, "seed {seed}: member {member}");
            let mut pending: Vec<(i64, i64)> = service.timers.iter().collect();
            let mut canon_pending: Vec<(i64, i64)> = canon
                .pending
                .iter()
                .map(|(id, deadline)| (*id, *deadline))
                .collect();
            pending.sort_unstable();
            canon_pending.sort_unstable();
            assert_eq!(
                pending, canon_pending,
                "seed {seed}: member {member}'s timers"
            );
            for (id, deadline) in pending {
                assert!(
                    deadline > owed_before,
                    "seed {seed}: timer {id}, due at {deadline}, is pending beside member \
                     {member} at {leader_now} by the leader's clock, member {leader}'s"
                );
            }
            drawn.passed_over += service.passed_over;
        }
    }
    // The leader's member holds no timer that was due before the cluster settled and
    // whose event it has yet to append. One it holds marked may be pending in no
    // service: a service that stopped before it told its member of a timer handed
    // over leaves it so, and the member replaces it when its table is full.
    for (id, deadline, marked) in besides[usize::try_from(leader).expect("an id")]
        .member
        .iter()
    {
        assert!(
            marked || deadline > owed_before,
            "seed {seed}: the leader, member {leader}, holds timer {id}, due at {deadline}, \
             at {leader_now} by its clock, and has appended no event for it"
        );
    }
    drawn.handed = canon.handed;
    assert!(
        drawn.events >= canon.events,
        "every committed event was appended here"
    );
    drawn
}

/// The seeds of this run hand every timer over once, and they reach what the search
/// is for: events passed over, handlers that do not take an entry, services and
/// members started again, and a cut-off leader's events.
#[test]
fn every_timer_is_handed_over_once_at_one_position_beside_every_member() {
    let length_ms = setting("TIMER_SEARCH_MS", 60_000);
    let seeds: Vec<u64> = match std::env::var("TIMER_SEARCH_FROM") {
        Ok(_) => {
            let from = setting("TIMER_SEARCH_FROM", 1);
            (from..from + setting("TIMER_SEARCH_COUNT", 1)).collect()
        }
        Err(_) => SEEDS.to_vec(),
    };
    let mut all = Drawn::default();
    for &seed in &seeds {
        let drawn = search(seed, length_ms);
        all.events += drawn.events;
        all.handed += drawn.handed;
        all.passed_over += drawn.passed_over;
        all.not_taken += drawn.not_taken;
        all.service_starts += drawn.service_starts;
        all.member_starts += drawn.member_starts;
        all.stale_leader_events += drawn.stale_leader_events;
        all.unsettled_members += drawn.unsettled_members;
        all.unsettled_clusters += drawn.unsettled_clusters;
        all.leader_cuts += drawn.leader_cuts;
        all.refreshes += drawn.refreshes;
        all.stalls += drawn.stalls;
        all.leaders_stalled += drawn.leaders_stalled;
        if drawn.unsettled_members > 0 {
            println!(
                "seed {seed}: the elections of {} of the three members had not closed at the \
                 end (work item 420){}; the seed cut its leader off {} times",
                drawn.unsettled_members,
                if drawn.unsettled_clusters > 0 {
                    ", so no quorum had settled and the seed's last checks were not made"
                } else {
                    ""
                },
                drawn.leader_cuts
            );
        }
    }
    println!(
        "seeds {seeds:?}: {} timer events appended, {} handed over, {} passed over by a service, \
         {} entries not taken, {} service starts, {} member starts, {} events of a leader that \
         was not the cluster's, {} times a service sent every pending timer again, {} members \
         whose election had not closed at the end, {} seeds with no settled quorum",
        all.events,
        all.handed,
        all.passed_over,
        all.not_taken,
        all.service_starts,
        all.member_starts,
        all.stale_leader_events,
        all.refreshes,
        all.unsettled_members,
        all.unsettled_clusters
    );
    assert!(
        all.unsettled_clusters * 20 <= u64::try_from(seeds.len()).expect("a count"),
        "seeds {seeds:?}: more than one seed in twenty ended with no settled quorum"
    );
    assert!(all.handed > 0, "seeds {seeds:?} handed no timer over");
    assert!(all.passed_over > 0, "seeds {seeds:?} passed no event over");
    assert!(all.not_taken > 0, "seeds {seeds:?} left every entry taken");
    assert!(
        all.service_starts > 0,
        "seeds {seeds:?} started no service again"
    );
    assert!(
        all.member_starts > 0,
        "seeds {seeds:?} started no member again"
    );
    assert!(
        all.refreshes > 0,
        "seeds {seeds:?} had no service send its timers again"
    );
    assert!(
        all.stale_leader_events > 0,
        "seeds {seeds:?} had no cut-off leader append an event"
    );
    assert!(
        all.leaders_stalled > 0,
        "seeds {seeds:?} stalled no leader's module, of {} stalls",
        all.stalls
    );
}
