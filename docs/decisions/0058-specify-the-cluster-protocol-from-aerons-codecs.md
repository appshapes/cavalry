# 0058. Specify the cluster protocol, version 0, from Aeron Cluster's codecs

- **Status:** Accepted
- **Date:** 2026-11-06
- **Invariants affected:** 1, 3, 5, 6
- **Supersedes:** none. Adds `spec/cluster.md` at version 0; changes no other page
  (ADR 0052, feature 2)

## Context

ADR 0052 makes the fifth slice Raft on the durable log and lists its second feature as
the cluster protocol, specified before any of `cavalry-raft` is written: the entries of
the cluster's log, the consensus messages between members, the session protocol between
a client and the cluster, the snapshot marker, the members string, the stream ids, the
leadership term and its position arithmetic, the timeouts and what a reader rejects,
under Aeron's names where the concept matches (`CLAUDE.md`, Protocol: spec first). Most
of the transport under it is done: `spec/udp.md` version 1 carries one publication to
every destination of a control channel under `min` flow control (ADR 0053), the
durable log records a stream at the positions the stream has (`spec/durable-log.md`),
and the C ABI at version 1 reaches both from five languages (ADR 0057). Four things
it lacks the page names as dependencies: no command of `spec/client-protocol.md` version
5 and no key of `spec/udp.md` version 1 creates a publication at a position, which
every term after the first needs for its log publication; `start recording` records
an image from its rebuild position when the command is handled, not from the
position the image joined at, so data that arrived first lands in no recording, where
a member's recording of the log must begin at the join position; a replay's
publication has a session id of its own, so a span a member records from a replay
carries that id in its frame headers; and nothing yet records which terms a member
has joined, Aeron's `RecordingLog`, which a member's vote and its election need to
survive a restart.

Aeron Cluster's protocol is its codecs schema, `aeron-cluster-codecs.xml`, id 111 at
version 17, read on 2026-11-06 at `master` (1.55.0-SNAPSHOT), with the election in
`Election.java` and `ElectionState.java`, the quorum rules in `ClusterMember.java`, the
sessions in `SessionManager.java` and `ClusterSession.java`, the leader's position
update in `ConsensusModuleAgent.java` and the defaults in `ConsensusModule.java` and
`AeronCluster.java`. The schema has grown over seventeen versions: fields added since a
version are optional, three message families are deprecated, and a third of its
messages serve Aeron's separate service container, its archive, its backup tool and its
dynamic membership, none of which Cavalry has. A member of Cavalry's cluster is the
`cavalry` daemon in a member's role, its service a subscriber through the ABI, its
recording the durable log of the same daemon (ADR 0052).

Three rules of this repository shape the page. Every layout carries a version a reader
rejects when it does not know it, and a page says what a reader rejects in the order
the checks run (`spec/README.md`). Replaying the same log produces the same state byte
for byte, so nothing a service reads may come from a member's own clock (invariant 1).
Every table has a bound and a documented behaviour when full (invariant 3). Issue 16,
read before the events were written, names two gaps of the client protocol: an
eviction with no event and an answer after a registration succeeded; the cluster's
session events are written so that neither recurs there: every close carries its
reason, and the bound on sessions is answered at the connect.

## Decision

`spec/cluster.md` version 0 specifies the cluster protocol as Aeron Cluster's, field for
field where the concept matches, under a header and a version of Cavalry's own. Every
message is the payload of one message on a `cavalry:udp` stream and begins with an
8-byte message header: block length, template id, schema id 1 and version 0, Aeron's
SBE header with Cavalry's schema id and this page's version; the fixed fields follow in
Aeron's order, every field present, and each variable field is a 4-byte length and its
bytes. The template ids are Aeron's: the session protocol's session message header,
session event, session connect, session close, session keepalive and new leader event
(1 to 6); the log's entries, a session message with its header, the timer event, the
session open and close events, the cluster action, the new leadership term event and
the membership change event (1, 20 to 25); the consensus messages, canvass position,
request vote, vote, new leadership term, append position, commit position, catch-up
position and stop catch-up (50 to 57); and the snapshot marker (100). The members
string is `id,ingress,consensus,log,catchup,archive` with members separated by `|`, at
most 8 members. The stream ids are Aeron's defaults: the log 100, ingress 101, egress
102, consensus 108. The leadership term id counts from 0 and only rises; a term's base
position is the log position at which its new leadership term event begins; an entry's
position is the position after its frame, `spec/log-buffer.md`'s arithmetic, so that
every member's log holds every entry at one position. The commit position is the
highest position a quorum of active members, the leader among them, has appended,
bounded by the leader's own, a member counting only once its last report was for the
leader's term and at or past its base, Raft's rule that a leader commits no older
term's entries by counting replicas, which departs from Aeron (Alternatives); the
quorum is half the members plus one; a member is active while its last append
position is younger than the leader heartbeat timeout. The
election is Aeron's state machine, its eighteen states and the transitions the page
lists, with a member voting for a candidate whose log is at least its own by term then
position. The cluster's timestamp is the leader's wall clock in milliseconds since the
Unix epoch, written by the leader into each entry and read from the log by every
member and service, never from a clock of their own. The timeouts are Aeron's defaults
as constants with no configuration key: leader heartbeat interval 200 ms and timeout
10 s, election timeout 1 s, election status interval 100 ms, startup canvass timeout
60 s, session timeout 10 s, session connect timeout 5 s, keepalive interval 500 ms,
`spec/client-protocol.md`'s, each measured on a monotonic clock of the process that
measures it, never a wall clock or the cluster's time, so that a step of a host's
clock moves no election, which departs from Aeron's cluster clock (Alternatives). A cluster holds at most 10 open sessions, Aeron's
default, and a member at most 10 pending answers, the redirects and refusals it has
yet to send, a connect beyond them dropped unanswered and counted; the leader stops
reading its ingress images while its claim on the log is back-pressured, buffering no
message and dropping none; a session message the leader ignores is counted, and a
client treats a message with no reply as possibly lost, the new leader event its
report of a truncated term. A reader rejects, in order, a message too short for its
header or block, a schema id not 1, a header version not 0, a template id this
version does not name, a template its stream does not carry, a block length not the
template's, a variable field that runs past the message or bytes left after the last,
a consensus message's protocol version of a major other than 0, a value outside an
enumeration, a non-ASCII byte in an ASCII field, a member id below 0 where none is
allowed, a position below 0 where none is allowed and a term id below −1; a session
connect's version is read whatever its major, so that the leader answers it with an
error, as Aeron's `SessionManager` does, and a client that reads a session event of
another major reports it; a version field's major is the value shifted right by 16
bits, arithmetic, so a negative value is rejected. Follower log replication, Aeron's
replication of what a follower lacks up to the next term's base, runs in this version
over the messages the page has: the follower sends catch-up position from its append
position, and the leader answers with replays of its recordings, beginning in the
recording that begins at or below the position and ends above it and ending at the
first term base log position in its log above the position, one `start replay` after
another where the span crosses a recording's end, which is Aeron's stop position on
the follower's side, the next term base log position when the append position is
below it, else the next log position; within the leader's term the catch-up is the
same replay following the live recording. A leader that reads a follower's canvass
position first stops any replay it has running to it, a replay counting as running
from `replay ready` to `replay stopped`, so that a follower that failed and
canvasses again never joins a stale replay; and a follower checks that an image of
a replay or of the live log joins at its append position, failing its election,
counted, when it does not, as Aeron's `verifyLogJoinPosition` does, so that a hole
never lies below its append position. One departure the client protocol forces: a replay's publication has
a session id of its own, so the follower finds either replay as the one image of
stream 100 on its `catchup` endpoint, and the span it records from it carries the
replay's session id in its frame headers. A member's log is therefore its recordings
of stream 100 in position order, one per term received live and one per span received
from a replay, its append position the end of the contiguous span they hold from the
log's start and each position read from the recording that begins first among those
holding it; every member's log holds every entry at the same position with the same
payload, and two members' logs differ in the session ids of their frame headers
alone. New leadership term's next fields name the lowest term in the leader's record
above the reported one, the leader's own when none lies between, with its base and its
end, −1 for all three when the reported term is the leader's own, a departure from
Aeron's lookup of the term one above the reported one, so that a returning leader
truncates no more than its uncommitted tail, a member whose log holds an uncommitted
tail of a term the leader's record skips truncates it to the next recorded term's base,
and a member two terms behind replicates one term at a time; a reader whose message
carries no next term for a position below the leader's term base fails its election. A member's log leadership term id, which decides its vote and which its append
position carries, is the highest term in its record of recorded terms, not the term
of its last entry, as Aeron's `logLeadershipTermId` is: a member records a term, with
its base, as the leader on entering leader log replication, as a follower when it
takes a new leadership term's sender as its leader with its log at the leader's term
base, when a replication ends, the term replicated and, at the leader's base, the
leader's term, when it replays a term's new leadership term event, and when it joins
the live log, in a record of terms and their base positions kept in its durable log
directory and written before it reports an append position for the term, so that a member that joined a term before its first
entry reached it outranks a longer log of an older term; with the leader recording
its term on entering leader log replication and a follower whose replication ends at
the leader's term base recording the leader's term before it reports the base, so
that only members that recorded the term count toward its commits, a committed entry
is never truncated, the Raft paper's figure 8 and its section 5.4.2. The leader's log publication of every
term after the first starts at the term base log position, which needs a publication
created at a position that the transport does not give yet: the version of `spec/udp.md` and
`spec/client-protocol.md` that adds one, under its own ADR, is card 227's first
change. Out of this version: the challenge and challenge
response, Aeron's authentication, with their template ids reserved; dynamic membership
and its deprecated messages; the service control protocol of a service in a process of
its own; the admin request; the backup, heartbeat and standby snapshot queries; the
snapshot's contents between its markers, which ADR 0052 leaves to card 230; the
members string's seventh and eighth fields, Aeron's response endpoints.

## Alternatives considered

- **Aeron's schema id 111 and version 17 on the wire, as `spec/udp.md` keeps Aeron's
  frames byte for byte.** Lost. The transport frames are the same bytes with the same
  meaning, so an Aeron receiver reads a Cavalry sender's datagram as its own and the
  other way, and the version byte stays 0 for that reason (ADR 0040, ADR 0053). The
  cluster messages are not: a Cavalry member carries every field as fixed where Aeron's
  schema marks the later ones optional, leaves out the messages Cavalry does not have,
  and answers a connect from a client of the other system with nothing it understands.
  A reader that announced schema 111 would take an Aeron member's messages for its own
  and be taken for one, and `spec/README.md` wants a version a reader of this page
  rejects when it is not its own. Schema id 1 and version 0 give the page its own
  number, and the Aeron sentence of the page says what the fields copy.
- **A protocol of Cavalry's own shape, Raft's `AppendEntries` and `RequestVote` alone.**
  Lost. Textbook Raft carries entries inside its append message and leaves catch-up,
  replay, snapshots and the service's view to the implementation; Aeron's shape puts
  the entries on one replicated stream and keeps the consensus messages to positions,
  which is what the durable log and multi-destination cast already give (ADR 0052),
  and Aeron's states name the catch-up and replay paths that a member joining late or
  restarting has to walk. Aeron's names and documentation then explain Cavalry's
  (`CLAUDE.md`, When in doubt), and ADR 0052 asked for them.
- **SBE's optional fields and `sinceVersion`, so that a later version adds a field
  without a new template.** Lost. Every page of `spec/` versions the whole layout and a
  reader rejects a version it does not know; a field added later is version 1 of this
  page with its vectors, as `spec/udp.md` moved to version 1 for a channel key. Every
  field is present in version 0, including those Aeron added over its versions, so a
  reader has one layout per template and the block length is a constant.
- **Aeron's cluster clock for the timeouts.** Lost. Aeron measures its cluster's
  timeouts on `ClusterClock.timeNanos()`, which `MillisecondClusterClock`, its default,
  takes from the wall clock; a step of a member's wall clock would start an election
  that nothing warranted or suppress one that was due. Every timeout of the page runs
  on a monotonic clock of the process that measures it instead, a member's or a
  client's, so that the wall clock moves no election; the cluster's time, which the
  log carries, stays a wall clock's milliseconds for the service.
- **Catch-up from any position in place of follower log replication, one replay
  across the terms a member missed.** Lost. One replay across terms has no stop
  position a follower can check term by term, and each span a member receives from a
  replay is a recording of its own, so the replication is Aeron's, one term at a time
  to the next term's base, over the catch-up messages the page has.
- **Follower log replication out of version 0, a member below the leader's term base
  log position failing its election until card 230.** Lost: a follower that is behind
  within the previous term is the ordinary case at failover, and a cluster whose
  surviving followers cannot replicate commits nothing until the old leader returns,
  which is the fall-over invariant 3 forbids.
- **One recording spanning terms, Aeron's extend recording.** Lost for this version: a
  recording holds one session id (`spec/durable-log.md`) and each term's publication
  and each replay has its own, so the page defines a member's log as its recordings in
  position order, one per term received live and one per replayed span, and leaves
  one recording across terms, and a replay that carries the log's session id, to the
  Revisit.
- **Recording a term only on joining its live log, as Aeron's `followerLogAwait`
  does.** Lost. A follower at the leader's term base goes to follower replay, replays
  only to the commit position it was told, below the base, and canvasses again, so
  under Aeron's rule it would record the leader's term only in follower log await,
  which it never reaches before the leader's commit position rises, which cannot rise
  until a quorum has recorded the term: an ordinary failover would never end. A
  follower therefore records the leader's term at the base as soon as it takes the
  leader from a new leadership term with its log at that base, the log a joiner in
  follower log await holds at the same position, and a replication that stops at the
  leader's base records the leader's term after the term replicated.
- **Aeron's quorum position, every active member's last position whatever term it was
  reported for.** Lost: invariant 1. Aeron's `quorumPositionBoundedByLeaderLog` ranks
  every active member, `leaderInit` records the leader's term before a follower has
  joined it, and a replication that stops at the leader's term base records the
  follower's old term, so a leader can commit an older term's entries from members
  that still record the older term, and a restarted member whose log ranks higher by
  its recorded term can win a vote and truncate them, the ninth review's example of a
  committed entry lost. The page counts a member toward a commit only once its last
  report was for the leader's term at or past its base, records the leader's term on
  entering leader log replication and at the end of a replication that reaches the
  base, and stays at the old commit position during an election until a quorum has
  recorded the new term, Raft's section 5.4.2, which Aeron's documentation does not
  explain and this ADR does.
- **A members string of up to eight fields, as Aeron parses.** Lost. The seventh and
  eighth are Aeron's archive response and egress response endpoints, for response
  channels Cavalry does not have; a string with more than six fields is a grammar
  error until a version names the seventh.
- **No bound on the members.** Lost: invariant 3. Eight members holds an odd cluster of
  up to seven, the largest Aeron's documentation treats as practical, and every
  per-member table of the slice, the ranked positions, the votes and the destinations,
  is sized by it.
- **A keepalive interval of the client's choosing, as Aeron's client leaves it to the
  application.** Lost. A fixed interval is one fewer thing a client of five languages
  can get wrong; 500 ms is `spec/client-protocol.md`'s keepalive interval for a 10 s
  liveness timeout, the same ratio, and a session's keepalives cost the leader nothing
  it does not already pay for the ingress image's status messages.
- **The member's own clock for the cluster's timestamp on a follower.** Lost: invariant
  1. A follower that stamped entries with its own clock would replay to a different
  state from the leader's; the leader stamps, the log carries, and a service reads the
  timestamp as an entry's field.
- **Aeron's next fields, the term one above the reported one when the leader's record
  holds it, else the leader's own term at its base.** Lost: invariant 1. Aeron's
  `Election.onCanvassPosition` looks up `logLeadershipTermId + 1` in a record that
  `RecordingLog.ensureCoherent` has filled in, one entry per term from the first to the
  leader's own, so the lookup never skips a term; a member of Cavalry records only the
  terms it received, live or by replication, so a leader's record has gaps. With
  Aeron's lookup over such a record, a follower whose log holds an uncommitted tail of
  term 2 at position 300, reporting term 2 to a leader of term 5 whose record holds
  terms 2 at 0, 4 at 200 and 5 at 1000, would be told next term 5 at 1000 and its tail
  left in place below the leader's base, in place of the entries term 4 committed there,
  the twelfth review's example. The lowest term above the reported one, 4 at 200 in
  that example, has the follower truncate to 200 and replicate term 4, and a leader's
  own record always holds its own term, so the field is never without a term; the
  `next-fields` and `new-leadership-term` vectors pin both.

## Evidence

None measured: a wire layout, a state machine and a set of rules, each Aeron's. What
informed it: Aeron's sources named in Context, read on 2026-11-06, with
`ConsensusModule.Configuration`'s defaults, `LEADER_HEARTBEAT_INTERVAL_DEFAULT_NS`
200 ms, `LEADER_HEARTBEAT_TIMEOUT_DEFAULT_NS` 10 s, `ELECTION_TIMEOUT_DEFAULT_NS` 1 s,
`ELECTION_STATUS_INTERVAL_DEFAULT_NS` 100 ms, `STARTUP_CANVASS_TIMEOUT_DEFAULT_NS` 60 s,
`SESSION_TIMEOUT_DEFAULT_NS` 10 s, `MAX_CONCURRENT_SESSIONS_DEFAULT` 10,
`LOG_STREAM_ID_DEFAULT` 100, `CONSENSUS_STREAM_ID_DEFAULT` 108, and
`AeronCluster.Configuration`'s `INGRESS_STREAM_ID_DEFAULT` 101,
`EGRESS_STREAM_ID_DEFAULT` 102 and `MESSAGE_TIMEOUT_DEFAULT_NS` 5 s;
`ClusterMember.quorumThreshold`, `quorumPosition`, `compareLog`, `isActive`,
`isUnanimousCandidate`, `isQuorumCandidate`, `isUnanimousLeader` and
`isQuorumLeader`; `ConsensusModuleAgent.updateLeaderPosition` and
`quorumPositionBoundedByLeaderLog`; `Election`'s per-state methods and `onCanvassPosition`;
`RecordingLog.ensureCoherent` and `findTermEntry`; `SessionManager`'s
connect, close and timeout checks; `ClusterMember.parse` and `ingressEndpoints`;
`MillisecondClusterClock` as the default clock; ADR 0052's Decision and card 226's
brief; issue 16. The change that accepts this ADR carries the page, its vectors from
the reference encoder `spec/vectors/cluster.py`, every message once valid and one
vector per rejection in the order the checks run, the members string parsed and
refused, and the arithmetic of positions, quorum and votes, the reference page, the
glossary rows and `cavalry-raft`'s reading list; the card's second pull request carries
the Rust codecs in `cavalry-proto` and the suite `just conformance` runs over the
vectors, the evidence the Done-when names.

## Consequences

Easier: the replicated log (card 227), the election (card 228), the cluster client
(card 229) and the catch-up and snapshot (card 230) each implement a page that exists,
with vectors, under names an Aeron reader knows; a reader rejects a message of another
version by its header before any field is read; every member's log holds every entry
at the same position with the same payload, so `just replay-check` extends to a
cluster as ADR 0052 asks, comparing entries and their payloads rather than frame
headers, whose session ids differ between members.

Harder: the page carries eighteen election states and their transitions before any
of them runs, so the implementing features must keep it true and amend it by a new
version when they find it is not; every field Aeron made optional is mandatory here,
so a value Aeron leaves null is written as its null value and the page says which
fields allow one; the consensus messages cross the wire as message payloads inside
data frames, so a reader walks `spec/frame.md` first and this page second. A member
at version N+1 must speak this page's header version 0 to a member at version N, since
a reader rejects any other, and card 231 decides how it does (ADR 0052, feature 7).
Before card 227 builds the replicated log, a version of `spec/udp.md` and
`spec/client-protocol.md` must let a publication be created at a position, Aeron's
`init-term-id`, `term-id` and `term-offset` keys or their equal, and let a recording
be armed on a subscription before its image arrives so that it begins at the image's
join position, as Aeron's Archive records each image of a channel, under its own ADR:
without the first no term after the first can start its log at the term base log
position, and without the second a follower's recording of the live log or of a
replay begins past the data that arrived first, a hole its append position never
passes. That version is card 227's first change, and the format of the record of recorded
terms, the page's fourth dependency, is card 227's as well, written with the
replicated log that reads it. A member rejoins by replicating one term at a time
from the leader's recordings, so a leader keeps the recordings of every term a
member may still lack, and a position retention purged is a replication the leader
cannot serve, counted in `cluster.log_replications_unavailable`, which `doctor` reads.
Card 230 may then change two pages: `spec/client-protocol.md`, for a `start replay`
that carries the log's session id so that a replay's frames are the log's bytes, and
`spec/durable-log.md`, for one recording that spans terms.

Revisit: when card 229's client needs the challenge, for Aeron's authentication
protocol and the two reserved template ids; when a member must be added to or removed
from a running cluster, for the membership change event's use and the messages Aeron
deprecated; when a service runs in a process of its own, for the service control
protocol, the schedule and cancel timer, the close session and the service ack; when
card 230 writes the snapshot, for its contents between the markers and the standby
snapshot message; when card 230 wants a replay that carries the log's session id, or
one recording that spans terms, so that two members' logs are the same bytes and a
replication can run from a peer other than the leader through the `archive`
endpoint; when a cluster of more than eight members is wanted; when `doctor` shows a host that needs
a timeout of its own, for the first configuration key of the slice with its
`why_not_default`.

## References

- `spec/cluster.md`; `spec/vectors/cluster.py`; `spec/README.md`;
  `docs/reference/cluster.md`; `docs/glossary.md`; `crates/cavalry-raft/README.md`
- ADR 0040, ADR 0052, ADR 0053, ADR 0057; `spec/udp.md`, `spec/frame.md`,
  `spec/log-buffer.md`, `spec/durable-log.md`, `spec/client-protocol.md`
- `CLAUDE.md`: invariants 1, 3, 5 and 6; Protocol; When in doubt
- Issue 16
- Aeron, read 2026-11-06 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/resources/cluster/aeron-cluster-codecs.xml` (schema id 111,
  version 17);
  `aeron-cluster/src/main/java/io/aeron/cluster/{Election,ElectionState,ClusterMember,ClusterSession,SessionManager,ConsensusModule,ConsensusModuleAgent,MillisecondClusterClock}.java`;
  `aeron-cluster/src/main/java/io/aeron/cluster/client/AeronCluster.java`
