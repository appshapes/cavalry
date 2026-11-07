# 0060. Replicate the log with an appointed leader, a term record and a per-peer consensus channel

- **Status:** Accepted
- **Date:** 2026-11-07
- **Invariants affected:** 1, 2, 3, 4, 5
- **Supersedes:** none. Takes `spec/durable-log.md` to version 2 with the terms file;
  changes no other page.

## Context

Card 227 builds the fifth slice's third feature (ADR 0052): the replicated log with an
appointed leader and no election. `spec/cluster.md` version 0 (ADR 0058) gives the
protocol, and ADR 0059 gave the transport what the log needs, a publication created at
a position and a recording armed on a subscription. Four decisions are left to this
feature's ADR, each with its evidence (ADR 0052, Decision): the format of a member's
record of recorded terms, `spec/cluster.md`'s fourth dependency, "kept in its durable
log directory and written before the member reports an append position for the term";
whether the consensus channel is one publication per peer or a multi-destination
channel of feature 1; the leader's own append position and whether the leader counts
toward the quorum before its recorder has synced; and the keys that name the cluster,
the members string and the member id, which `cavalry run` reads to start the
consensus module in a member's role.

The record of terms is what a member's vote, its election and its commit position
stand on (`spec/cluster.md`, "The log"): a term, with its term base log position, is
recorded when the member joins or replicates it, truncation drops the terms above the
position, the log leadership term id is the highest term recorded, and the candidate
term, the highest term the member has voted in or proposed, is stored before the vote
or request vote that raises it is sent. In Aeron, this is `RecordingLog`, a text-free
binary file of entries, each a recording id, a leadership term id, a term base log
position, a log position, a timestamp and a type, and `NodeStateFile`, which holds the
candidate term; Cavalry keeps one file for both, since a member writes them from one
thread and reads them at one moment, its start.

## Decision

The log directory gains a `terms` file, `spec/durable-log.md` version 2, "Terms": a
64-byte header, the magic `CVLYTERM`, a version of 0, the entry length 32, the
candidate term as an i64 with its CRC32, and then 32-byte entries, each a state, a
CRC32 of its other fields, the leadership term id, the term base log position and the
recording id of the term's recording in this directory, −1 while the term has none.
The writer appends an entry by writing its fields, then its checksum, then its state,
1, last, and syncs; truncates by writing state 3 over each entry whose base is above
the position and syncing; raises the candidate term by writing the value, then its
checksum, and syncing; and a reader takes entries in file order, skipping state 3,
stopping at the first entry whose state is 0 or whose checksum fails, the torn tail of
a crash. The file holds at most 1048576 entries, the catalog's bound on recordings,
and `cavalry doctor` warns from 90 percent of it; a member refuses to record a term
past the bound with `terms full`, and the operator starts a new directory, as for the
catalog. The recovery pass leaves the file alone: it names files by recording id, and
`terms` is none.

The consensus channel is one unicast `cavalry:udp` publication per peer, on stream 108
to the peer's `consensus` endpoint, as `spec/cluster.md`'s table says, not a
multi-destination channel of feature 1: a member sends different messages to different
peers, append position to its leader and request vote to each member once, and the
leader's commit position must reach every live follower whatever one dead follower
does, which `min` flow control on one channel would not allow and `max` would allow
only by letting the slowest follower lose it.

A member's append position, the leader's among them, is its recording's synced
position, `recording.synced_position`, what a sync has made durable, not its producer
position and not what the operating system holds: a member counts toward a commit
only for what survives its own crash, Raft's rule that an entry is on stable storage
before its holder acknowledges it (§5.3), and the leader counts toward the quorum as
any member does, once its recorder has synced the term's entries. The recorder syncs
once per second (`spec/durable-log.md`, "The driver"), so a commit waits up to a second
behind the leader's writes at the quietest; the consensus module asks the recorder to
sync after each append it reports, the bounded request queue the conductor already
holds, so that a commit waits for one sync and not the next tick.

Two configuration keys name the cluster: `cluster.members`, the members string of
`spec/cluster.md`, "Members and streams", `id,ingress,consensus,log,catchup,archive`
per member separated by `|`, empty by default, which is no cluster and a driver that
runs as before; and `cluster.member.id`, this member's id in that string, −1 by
default. Both are set once in a unit file or a container's environment, so neither has
a flag; each has its `why_not_default` and a `doctor` check, `cluster-members` and
`cluster-member-id`, that resolves every endpoint, refuses a string `parse_members`
refuses, an id not in the string, and one key set without the other. Until card 228's
election, the leader is the member with the lowest id in the string, appointed by the
string's own order: a key the next feature removes would need a `why_not_default` that
is not true.

`cavalry run` with `cluster.members` set starts the consensus module as the driver's
sixth dedicated thread, `cavalry-raft`'s, with the driver's idle strategy, linked to
the conductor by SPSC queues as the recorder, the sender and the receiver are: the
leader creates its log publication on the control channel of `spec/cluster.md`'s table
at the term base log position, through the position keys, with every other member's
`log` endpoint as a destination; each follower subscribes to its `log` endpoint on
stream 100 and arms a recording on it before the leader's image arrives; followers
report append positions and the leader publishes the commit position over the
consensus channel and as the `cluster.commit_position` counter. The service reads the
committed log through a subscription on the member's `log` endpoint and stream 100,
polled no further than the commit position, Aeron's bounded poll, through a client
API and ABI call this card adds in a change of its own, so that a service is a program
in any of the five languages.

## Alternatives considered

- **Aeron's two files, `RecordingLog` and `NodeStateFile`.** Lost. A member writes both
  from its consensus thread and reads both at start, and `spec/cluster.md` needs the
  candidate term stored before a vote as it needs a term recorded before an append
  position; one file with one header, one sync per write and one reader keeps the two
  from disagreeing after a crash between them.
- **The record of terms in the catalog, a field per descriptor.** Lost: a term is not a
  recording. A term replicated from a replay lands in a recording of its own, a term
  with no entries yet has no recording, and a descriptor is a recording's, written by
  the recorder, while a term is the consensus module's.
- **Deleting a truncated entry by rewriting the file.** Lost: a rewrite that crashes
  midway loses every term, while a state written in place loses at most the one entry
  being marked, which the reader then finds valid or torn, both of which the member
  can live with: a term marked deleted again, or a term it will record again.
- **The consensus channel as one multi-destination channel.** Lost, above: the
  messages differ per peer, and the leader's heartbeat must outlive one dead follower.
- **The producer position as a member's append position.** Lost: invariant 1. A
  member whose entries are in the page cache and not on disk would count toward a
  commit the cluster then loses with the member's host, the case Raft's stable storage
  rule exists for.
- **A key that appoints the leader.** Lost: card 228's election removes it, and a key
  lives in `config.toml` with a reason its default cannot be right, which this one
  would lack.
- **The service reading the log through the consensus module, a queue of committed
  entries.** Lost: invariants 2 and 6. The service's path is the subscriber's path of
  every slice before (ADR 0052), a log buffer read by position, with the commit position
  as its bound; a queue would copy every entry and give the service an API of its own.

## Evidence

None measured. What informed it: `spec/cluster.md`, "The log", "Members and streams"
and "Roles and the election" (ADR 0058); ADR 0059's position keys and armed recording;
Aeron's `RecordingLog` and `NodeStateFile`, read 2026-11-07, for the fields a term
record holds; the Raft paper, §5.3, for the stable storage rule; `spec/durable-log.md`'s
catalog and its slot creation order, which the terms file copies; and the card's brief.
What the implementation proves goes in the commit messages: the terms file read back
after each write and after a torn tail, the loopback cluster of three replicating
100,000 messages with no committed entry lost, and the replay check across members.

## Consequences

Easier: a member starts from one file that holds every term it recorded and the last
term it voted in; the leader's commit position is a quorum of synced positions, so a
committed entry survives a crash of any minority; a service in any language reads the
committed log as it reads any stream, bounded.

Harder: the log directory holds one file more, which `cavalry doctor` reads and the
recovery pass must leave alone; a commit waits for a sync on a quorum, up to a second
on an idle member plus the request the consensus module sends; the driver gains a
sixth thread and two keys; the client API and the ABI gain a bounded poll.

Revisit: when card 228's election writes the candidate term under contention, for the
file's write ordering; when card 230's snapshots need an entry of their own, Aeron's
snapshot entry type, for a type field; when a recorder syncs faster than once a
second, for the commit latency; when the first release freezes the file's version.

## References

- `spec/durable-log.md`, version 2, "Terms" and "The driver".
- `spec/cluster.md`, "The log" and "Members and streams"; ADR 0058; ADR 0059.
- ADR 0052, Decision, feature 3, and its decisions left to this ADR.
- Aeron's `io.aeron.cluster.RecordingLog` and `io.aeron.cluster.NodeStateFile`.
- Ongaro and Ousterhout, "In Search of an Understandable Consensus Algorithm", §5.3.
