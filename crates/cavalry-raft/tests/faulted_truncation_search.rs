//! A search for a member that leads or joins on an end its truncation no longer holds
//! (`spec/cluster.md`, "Messages during an election"; ADR 0148; CLAUDE.md, invariant
//! 1). Three members in memory under a delivery seed of loss and delay, with the
//! leader cut off from both others for 11 s to 19 s, a leader halted within 300 ms of
//! its election, members halted and resumed, parted one way or both, started again on
//! the log directory they kept, their recordings ended, and their modules stalled
//! while their drivers run on, the leader's two times in three while one leads
//! (`Cluster::stall`; work item 467), each at a moment the seed draws; and truncations
//! made to fault, at a member the seed draws, at a step it draws, the second, the file
//! or the record of terms, for a number of attempts in a row it draws. A leader cut
//! off appends a tail that the leader the others elect lacks, and truncates it when it
//! comes back, so the seeds truncate, and fault, often. A leader halted just after its
//! election can leave the member it named a truncation to the most up to date of those
//! left, which is when a member whose truncation faulted could lead or join on the end
//! it no longer holds.
//!
//! The check of every step is `elected_cluster`'s, and three more of every member: it
//! never added a log, joined one or subscribed for a replay while its log was cut
//! short of the end it reported; its log has no gap, which over a driver would be a
//! recording listed past the end of the one before it, refused at its next start
//! (`listed-recording`); and it does not lead while it owes a truncation.
//!
//! Each run of `just test` searches the seeds of [`SEEDS`]. A wider search names its
//! own: `FAULTED_TRUNCATION_SEARCH_FROM`, `FAULTED_TRUNCATION_SEARCH_COUNT` and
//! `FAULTED_TRUNCATION_SEARCH_MS`, for example 3,000 seeds of 60 s each in a release
//! build, as ADR 0148's Evidence ran.

mod elected_cluster;

use cavalry_raft::Role;
use cavalry_raft::memory::{Cluster, TruncationFault};
use elected_cluster::Safety;

/// The members.
const IDS: [i32; 3] = [0, 1, 2];

/// The seeds `just test` runs: seed 1, and seeds 226, 411 and 609, the first three at
/// which the search, with the rule of ADR 0148 taken out, finds a member that joins a
/// log while its log is cut short of the end it reports. Seeds 96 and 482 did so until
/// the stalls of work item 467 moved the search's paths, and seed 489 until work item
/// 476's start of a new cluster did.
const SEEDS: [u64; 4] = [1, 226, 411, 609];

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

    /// A value below `bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// One of `items`.
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

/// What one seed's run saw.
#[derive(Default)]
struct Seen {
    /// Leaders cut off from both others.
    isolations: u64,
    /// Leaders halted within 300 ms of their election.
    new_leaders_halted: u64,
    /// Entries the cluster took.
    entries: u32,
    /// Truncations that finished, owed or not.
    truncations: u64,
    /// Truncations that became owed.
    faulted: u64,
    /// Owed truncations that then finished, the member running all along.
    finished: u64,
    /// Modules that stood still while their drivers ran on.
    stalls: u64,
    /// Of them, a leader's.
    leaders_stalled: u64,
}

/// The three checks of this search, on every member, after a step.
fn check(cluster: &Cluster, seed: u64, ms: u64) {
    for id in IDS {
        assert!(
            !cluster.took_a_log_cut_short(id),
            "seed {seed}, {ms} ms: member {id} added a log, joined one or asked for a \
             replay while its log was cut short of the end it reported"
        );
        assert_eq!(
            cluster.log_gap(id),
            None,
            "seed {seed}, {ms} ms: member {id}'s log has a gap"
        );
        let member = cluster.member(id).expect("a member");
        assert!(
            member.truncation_owed() < 0 || member.role() != Role::Leader,
            "seed {seed}, {ms} ms: member {id} leads while it owes a truncation to {}",
            member.truncation_owed()
        );
    }
}

/// Runs `seed` for `length_ms`, the checks of every step failing the test where they
/// fail, and returns what it saw.
fn search(seed: u64, length_ms: u64) -> Seen {
    let mut safety = Safety::new(&IDS);
    let mut cluster = Cluster::with_election(&IDS, seed).expect("three members");
    let mut draw = Draw(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let delay = draw.pick(&[0, 0, 20, 200, 2_000]);
    let loss = draw.pick(&[0, 0, 200, 30]);
    cluster.set_delivery(loss, delay);
    let mut seen = Seen::default();
    let mut halted: Option<(i32, u64)> = None;
    let mut stalled: Option<(i32, u64)> = None;
    let mut parted: Option<(i32, i32, u64)> = None;
    let mut isolated: Option<(i32, u64)> = None;
    // Per member: the count of truncations faulted last read, and whether it owed one.
    let mut faulted = [0_u64; 3];
    let mut truncated = [0_u64; 3];
    let mut owed = [false; 3];
    let mut entry: u32 = 0;
    // The leader last seen, and a halt drawn for a leader just elected.
    let mut last_leader: Option<i32> = None;
    let mut halt_new_leader: Option<(i32, u64)> = None;
    for ms in 0..length_ms {
        if draw.below(10) == 0 && cluster.offer(&entry.to_le_bytes()).is_ok() {
            entry += 1;
        }
        if let Some((member, until)) = halted
            && ms >= until
        {
            cluster.resume(member);
            halted = None;
        }
        if let Some((member, until)) = stalled
            && ms >= until
        {
            cluster.unstall(member);
            stalled = None;
        }
        if let Some((a, b, until)) = parted
            && ms >= until
        {
            cluster.heal_between(a, b);
            parted = None;
        }
        if let Some((member, until)) = isolated
            && ms >= until
        {
            cluster.heal(member);
            isolated = None;
        }
        // A leader stopped within 300 ms of its election, in one election of three: the
        // member it named a truncation to may then be the most up to date of those
        // left, before any other has recorded the new term, which is when one whose
        // truncation faulted could lead or join on the end it no longer holds.
        let leader = cluster.leader();
        if leader != last_leader {
            if let Some(new) = leader
                && halt_new_leader.is_none()
                && draw.below(3) == 0
            {
                halt_new_leader = Some((new, ms + draw.below(300)));
            }
            last_leader = leader;
        }
        let is_stalled = |id: i32| stalled.is_some_and(|(member, _)| member == id);
        if let Some((member, at)) = halt_new_leader
            && ms >= at
        {
            if halted.is_none() && !is_stalled(member) {
                cluster.halt(member);
                halted = Some((member, ms + 2_000 + draw.below(10_000)));
                seen.new_leaders_halted += 1;
            }
            halt_new_leader = None;
        }
        let is_halted = |id: i32| halted.is_some_and(|(member, _)| member == id);
        match draw.below(20_000) {
            0..=9 if isolated.is_none() => {
                // Long enough for the leader to give up the lead, at the leader
                // heartbeat timeout, and for the others to elect: the cut leader's
                // tail is then one the new leader lacks, truncated when it ends.
                if let Some(leader) = cluster.leader() {
                    cluster.cut(leader);
                    isolated = Some((leader, ms + 11_000 + draw.below(8_000)));
                    seen.isolations += 1;
                }
            }
            10 if halted.is_none() => {
                let member = draw.pick(&IDS);
                if !is_stalled(member) {
                    cluster.halt(member);
                    halted = Some((member, ms + 300 + draw.below(10_000)));
                }
            }
            11 if parted.is_none() => {
                let a = draw.pick(&IDS);
                let b = draw.pick(&[(a + 1) % 3, (a + 2) % 3]);
                let until = ms + 200 + draw.below(10_000);
                if draw.below(2) == 0 {
                    cluster.cut_one_way(a, b);
                } else {
                    cluster.cut_between(a, b);
                }
                parted = Some((a, b, until));
            }
            12 => {
                let member = draw.pick(&IDS);
                if !is_halted(member) {
                    safety.restart(&mut cluster, member);
                    // A start ends a stall: the process that stood still is gone.
                    if is_stalled(member) {
                        stalled = None;
                    }
                    if let Some(index) = IDS.iter().position(|&id| id == member) {
                        faulted[index] = 0;
                        truncated[index] = 0;
                        owed[index] = false;
                    }
                }
            }
            13..=49 => {
                let member = draw.pick(&IDS);
                cluster.end_recording(member);
            }
            50..=249 => {
                let member = draw.pick(&IDS);
                let at = draw.pick(&[
                    TruncationFault::SecondStep,
                    TruncationFault::List,
                    TruncationFault::Terms,
                ]);
                let times = draw.pick(&[1, 1, 3, 30, 300, 3_000]);
                cluster.fault_truncation(member, at, times);
            }
            // A module that stands still while its driver runs on (work item 467): the
            // leader's two times in three while one leads (the leader on half the draws,
            // and a third of the rest), so that it stalls with entries appended after its
            // duty cycle read its append position. Its recording syncs, its replays and
            // its sender go on, and what is sent to it waits, for 1 ms to 20 s: within a
            // heartbeat interval, or long enough that the others elect without it.
            250..=256 if stalled.is_none() => {
                let member = match cluster.leader() {
                    Some(leader) if draw.below(2) == 0 => leader,
                    _ => draw.pick(&IDS),
                };
                if !is_halted(member) {
                    let span = draw.pick(&[2, 20, 200, 2_000, 20_000]);
                    if cluster.leader() == Some(member) {
                        seen.leaders_stalled += 1;
                    }
                    cluster.stall(member);
                    stalled = Some((member, ms + 1 + draw.below(span)));
                    seen.stalls += 1;
                }
            }
            _ => {}
        }
        safety.run(&mut cluster, 1);
        check(&cluster, seed, ms);
        for (index, &id) in IDS.iter().enumerate() {
            let member = cluster.member(id).expect("a member");
            let count = member.counts().log_truncations;
            seen.truncations += count.saturating_sub(truncated[index]);
            truncated[index] = count;
            let count = member.counts().truncations_faulted;
            seen.faulted += count.saturating_sub(faulted[index]);
            faulted[index] = count;
            let owes = member.truncation_owed() >= 0;
            if owed[index] && !owes {
                seen.finished += 1;
            }
            owed[index] = owes;
        }
    }
    seen.entries = entry;
    seen
}

/// The seeds of this run lead and join on no end a truncation cut, leave no gap in a
/// log and lose nothing that was committed; and they fault truncations on the way and
/// see them finish: a search that faulted none would show nothing.
#[test]
fn no_member_leads_or_joins_on_an_end_its_faulted_truncation_cut() {
    let length_ms = setting("FAULTED_TRUNCATION_SEARCH_MS", 60_000);
    let seeds: Vec<u64> = match std::env::var("FAULTED_TRUNCATION_SEARCH_FROM") {
        Ok(_) => {
            let from = setting("FAULTED_TRUNCATION_SEARCH_FROM", 1);
            (from..from + setting("FAULTED_TRUNCATION_SEARCH_COUNT", 1)).collect()
        }
        Err(_) => SEEDS.to_vec(),
    };
    let (mut faulted, mut finished, mut leaders_stalled) = (0, 0, 0);
    for &seed in &seeds {
        let seen = search(seed, length_ms);
        println!(
            "seed {seed}: {} entries, {} leaders cut off, {} new leaders halted, {} \
             truncations, {} faulted, {} finished after, {} modules stalled, {} of them a \
             leader's",
            seen.entries,
            seen.isolations,
            seen.new_leaders_halted,
            seen.truncations,
            seen.faulted,
            seen.finished,
            seen.stalls,
            seen.leaders_stalled
        );
        faulted += seen.faulted;
        finished += seen.finished;
        leaders_stalled += seen.leaders_stalled;
    }
    assert!(faulted > 0, "seeds {seeds:?} faulted no truncation");
    assert!(
        leaders_stalled > 0,
        "seeds {seeds:?} stalled no leader's module"
    );
    assert!(
        finished > 0,
        "no truncation of seeds {seeds:?} that faulted finished"
    );
}
