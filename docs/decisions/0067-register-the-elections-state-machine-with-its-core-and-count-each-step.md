# 0067. Register the election's state machine with its core, and count each of its steps

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 4, 6
- **Supersedes:** none. Amends ADR 0063 in one clause, which counters the election
  moves, and adds two rows to `spec/cluster.md`, "Counters", whose version stays 0:
  a counter is not a field of a message.

## Context

Card 227's core, `cavalry_raft::Consensus`, holds a member's election as
`ElectionState`: the eleven states of the appointed leader's path of
`spec/cluster.md` (ADR 0063). The core keeps a count for each counter that
`spec/cluster.md`, "Counters", names for its transitions, in `Counts`. The module that
stores those counts in the counters file, as client counters (ADR 0062), is the
crate's next change.

The first version of the core marked `ElectionState` `// not a state machine:` and
left its registration in `counters.toml` to the module. The review of pull request 171
refused that, for three reasons. `docs/reference/counters.md` defines the mark as "the
reason no transition of it needs a counter", and every transition of this enum needs
one. `docs/how-to/implement-a-feature.md`, step 3.3, says that a feature with a state
transition waits for its counters. And nothing would have made the next change remove
the mark, so `just lint-counters` passed because it was bypassed, not because it was
met.

Registering the machine found nine transitions that move no counter at all, in
`Counts` or in the spec's table: leader log replication to leader replay, leader
replay to leader init, leader init to leader ready, leader ready to closed, canvass to
follower replay, follower replay to follower log init, follower log init to follower
log await, follower log await to follower ready, and follower ready to closed.
`cluster.election_state` is a gauge, and a gauge that moves back hides how often it
moved, so it cannot be a transition's counter (`just lint-counters`, the `counter`
check). Invariant 4 says every state transition increments a counter.

## Decision

The machine is registered with the core, now. `counters.toml` has
`[[state_machine]] name = "cluster_election"` with `ElectionState`'s eleven states and
one `[[transition]]` row for each move the core makes and each counter the move adds
one to, and the enum carries `// state machine: cluster_election`. The eleven counts
of `Counts` are registered as client counters, ids 110 to 120, keyed by the member id
as `cluster.commit_position` is. `Counts::by_counter` pairs each count with the
`CounterId` that `cavalry-shm` generates for it, so that the core names every counter
it moves, which the lint's `moved` check asks, and the module stores what that method
returns. `cavalry-raft` takes `cavalry-shm`, a workspace member, as a dependency for
those constants.

Two new counters count the nine transitions. `cluster.elections_closed` counts the two
into closed: an election that closed, on the leader or on a follower.
`cluster.election_steps` counts the other seven: a step from one state to the next
that neither begins the election, fails it, closes it nor makes the member a leader.
With them, every election a member begins is accounted for: `cluster.elections` is
`cluster.elections_closed` plus `cluster.elections_failed`, plus one while an election
is under way. The two rows `spec/cluster.md`, "Counters", gains are:

| Counter | What it counts |
| --- | --- |
| `cluster.election_steps` | a step of an election from one state to the next that neither begins it, fails it, closes it nor makes the member a leader: canvass to follower replay, follower replay to follower log init, follower log init to follower log await, follower log await to follower ready, leader log replication to leader replay, leader replay to leader init and leader init to leader ready |
| `cluster.elections_closed` | an election that closed: leader ready to closed, the term's first entry appended, or follower ready to closed, the append position sent |

## Alternatives considered

- **Keep the mark until the module registers the machine.** Lost. The mark would say
  something false about the enum, and the gate would pass a feature with nine
  uncounted transitions. Registration costs the core nothing it does not already do:
  its counts exist, and naming their ids is a table of constants.
- **One counter for every transition, counted in the one function that moves the
  state.** Lost. Every transition that a named counter already counts would count
  twice, and an operator would learn from it only that the election moved, which the
  `cluster.election_state` gauge already shows. A count of closed elections answers a
  question the gauge cannot: how many elections ended well.
- **One counter for each of the nine transitions.** Lost. Nine names for moves the
  gauge already locates, none with a separate action for an operator. The lint allows
  one counter for several transitions, as `counters.allocations` serves two of
  `counters_record`'s. Aeron counts none of these moves; it has the election state and
  an election count.
- **Define the counter ids in `cavalry-raft` instead of depending on `cavalry-shm`.**
  Lost. Two places would hold one id, and nothing would check that they agree. The
  lint's `moved` check names the generated constant so that the code that counts and
  the registry cannot differ. The module reaches `cavalry-shm` through
  `cavalry-client` in any case (ADR 0063).

## Evidence

- `crates/cavalry-raft/tests/transitions.rs` reads the `cluster_election` rows of
  `counters.toml`, drives a leader of three, a cluster of one and six followers
  through every path the core has, and checks after every single call, a duty cycle
  or a message read, that a move it made is a registered row and that a counter
  registered for the move rose. It fails on a row no path made. With one step's count
  removed from the core, it fails with `leader_replay -> leader_init added one to none
  of its counters`.
- `tests/replication.rs`: three members that start together each close one election,
  in three steps on the leader and four on each follower.
- `just lint-counters`: `121 counters, 8 state machines, 2 generated files current,
  ok`.

## Consequences

- The core cannot gain a state or a transition without its row and its counter: the
  lint refuses an enum whose variants are not the machine's states, and the test
  refuses a move that is not a row.
- The module stores eleven client counters per member from `Counts::by_counter`, plus
  the gauges `spec/cluster.md` names, which it registers itself.
- `cavalry-raft` depends on `cavalry-shm`, the crate with the unsafe code. The core
  uses its constants only.
- Card 228's election adds states 2 to 4 and 11 to 13, and card 230's catch-up adds
  state 9. Each adds its variants, its rows and the counters of its transitions in the
  same change. Revisit the two counters here if an operator needs to tell one step
  from another by counters alone, which the gauge does today.

## References

- `spec/cluster.md`, "Roles and the election", "The appointed leader" and "Counters".
- `docs/reference/counters.md`, "State machines", `cluster_election`.
- [ADR 0012](0012-lay-out-the-counters-file.md), the counters file and `counters.toml`.
- [ADR 0062](0062-add-a-clients-counter-with-aerons-counter-commands.md), a client's
  counters.
- [ADR 0063](0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  the core and the module.
- Pull request 171, its reviews of 9705243 and 59069bb.
