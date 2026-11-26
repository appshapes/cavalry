# Cluster protocol, version 3

The cluster protocol makes three or more drivers one replicated state machine: the
cluster's log is one stream, written by the member that is the leader and sent to
every other member, a follower, over multi-destination cast (`spec/udp.md`, "The
sender"); every member records the log into its durable log (`spec/durable-log.md`) so
that every member's recording holds every entry at the same position; the leader commits
an entry once a quorum has recorded it; a service, a subscriber through the client API
and the C ABI, reads the committed log and nothing else; a client reaches the service
through a session with the leader; and when the leader is lost the members elect
another. In Aeron, this is Cluster, whose codecs schema, id 111 at version 17, these
messages copy field for field where the concept matches, and whose election the
members run (ADR 0058; `docs/glossary.md`). This page is the members and the streams
between them, the bytes of every message, the entries of the log and their positions,
the election, the sessions, the timeouts, and what a reader rejects. Nothing here
changes the transport: every message crosses the wire inside data frames
(`spec/frame.md`) on a `cavalry:udp` stream, and a reader walks that page first and
this one second.

Version 1 adds the rule for a cluster whose members are of more than one version of
this page, and the upgrade that makes such a cluster ("Roles and the election",
"Versions and the upgrade"; ADR 0136). It adds no message, no field and no entry:
every layout of version 0 is unchanged and every header's version stays 0, so a
member of version 0 reads every message of a member of version 1, and the two elect,
replicate and commit as one cluster. A member and a client of version 1 write 256,
this version in the semantic form of "Encoding", where those of version 0 wrote 0:
in canvass position, request vote, session connect and session event.

Version 2 adds a service's timers ("The service", "Timers"; ADR 0151): schedule
timer and cancel timer, which a service sends the member on its driver, the timer
record of a service's snapshot, and a use for the timer event, which every version
of major 0 names and no leader of versions 0 and 1 appends. No layout of version 1
changes and every header's version stays 0. It says what a version of a major may
add, and when a leader may append an entry that an earlier version does not append
("Versions and the upgrade"). A member and a client of version 2 write 512.

Version 3 adds the floor answer ("A member that lost its log"; ADR 0163). A member
that holds a vote floor asks its leader for one by bit 1 of append position's `flags`,
a bit every version of major 0 reserves and ignores, and a leader of version 3
answers with a floor answer, template 58, which it sends only to a member that
asked, so no member of an earlier version is sent it. A member of version 3 clears
its floor only once its log is also at or past an answer that names its own
consensus session. No layout of version 2 changes and every header's version stays
0. It adds a use for a reserved bit of a `flags` field to what a version of a major
may add ("Versions and the upgrade"). A member and a client of version 3 write 768.

## Members and streams

A cluster is a members string, the same string on every member and every client,
but for which of its members it marks a standby while that is changed ("Roles and
the election", "Changing a standby's mark"):

```text
<id>,<ingress>,<consensus>,<log>,<catchup>,<archive>[,standby]|<id>,<ingress>,...
```

Each member is six fields separated by `,`, or seven, members separated by `|`. `id`
is the member id, an integer from 0 upward, unique within the string; the next five
are endpoints, `host:port`, a host name or an address literal, an IPv6 literal in
square brackets, and a port from 1 to 65535, each resolved as `spec/udp.md`, "The
channel URI", resolves an endpoint. A seventh field is the word `standby`, and marks
the member a standby: a member that follows the log and has no vote ("Roles and the
election", "Standbys"; ADR 0134). A member without the mark is a voter. A string
holds at most 8 members ("Values"), at least one of them a voter. The endpoints are
where a member receives:

| Endpoint | Who receives on it | What |
| --- | --- | --- |
| `ingress` | the member, on the ingress stream | a client's session connect, session messages, keepalives and closes ("The session protocol") |
| `consensus` | the member, on the consensus stream | the consensus messages of every other member |
| `log` | the member, on the log stream | the leader's log; the leader's own `log` endpoint is the control address of its log publication |
| `catchup` | the member, on the log stream | the leader's replay of the log to a member that is behind, from the position it asked for |
| `archive` | the member | a peer's request to replicate its recording of the log; named here, used by no message of this version |

The streams are `cavalry:udp` streams, but for the service log, the log replay, the
consensus module stream and the service snapshot replay, with these stream ids,
constants in this version:

| Stream | Stream id | Publication | Subscriptions |
| --- | --- | --- | --- |
| log | 100 | the leader's, `cavalry:udp?control=<its log endpoint>\|control-mode=manual\|fc=min`, every other member's `log` endpoint added as a destination (`spec/client-protocol.md`, "Commands", add destination), created at the term base log position for every term after the first, by the channel's position keys, `init-term-id`, `term-id` and `term-offset` (`spec/udp.md`, "The sender"; ADR 0059); a member's recording of an image of this stream begins at the image's join position, so it is armed on the subscription before the image arrives, a `start recording` on a channel the driver has a subscription on and no image of yet (`spec/client-protocol.md`, "Commands", start recording; ADR 0059), since a recording started once the image is there begins at the image's rebuild position and the data that arrived before it lands in no recording; a member's two commands, `add subscription` and then `start recording`, leave a gap an image can arrive in, so a member takes where a recording begins from the recording's start position in the catalog (`spec/durable-log.md`, "Descriptor") and never from its image, and tells its election that position as the image's join position: a recording that began past the image's join is then an image that joined past the member's append position, which state 15 fails on, and it is no part of the member's log (ADR 0093) | each other member's, `cavalry:udp?endpoint=<its log endpoint>`, recorded into its durable log; a member catching up, `cavalry:udp?endpoint=<its catchup endpoint>` |
| consensus | 108 | one per member per peer, `cavalry:udp?endpoint=<the peer's consensus endpoint>`, an exclusive publication that the member adds once in each run of its consensus module, so that its session id is that run's alone ("The floor answer"; ADR 0163) | each member's, `cavalry:udp?endpoint=<its consensus endpoint>` |
| ingress | 101 | a client's, `cavalry:udp?endpoint=<a member's ingress endpoint>` | each member's, `cavalry:udp?endpoint=<its ingress endpoint>` |
| egress | 102 | the leader's, one per open session; the service's that answers the term's entries, the one beside the member that led the term, one per open session, for its replies ("Replies"); and any member's, one per connect it answers with a redirect or a refusal, at most the pending answers of "Values" at a time; each on the client's response channel | the client's, on its response channel |
| service log | 104 | the member's driver's, one for each service: a replay of the member's recording of the log that follows it, on `cavalry:ipc` ("The service") | the service's, on `cavalry:ipc` |
| log replay | 103 | the member's driver's, one: a replay of the member's recordings of the log that follows them, on `cavalry:ipc`, which the member's own consensus module reads as a service reads the service log, for its sessions ("Sessions"; ADR 0116). A stream of its own, so that this replay and a service's never reach each other's subscription | the consensus module's, on `cavalry:ipc` |
| consensus module | 105 | a service's, on `cavalry:ipc`, from its first open of its member's log to its close ("What a service tells its member"; ADR 0168): the service acks by which it tells the member on its driver of a snapshot it took ("Snapshots"; ADR 0131), and the schedule timer and cancel timer by which it tells that member of its timers ("Timers"; ADR 0151) | the member's consensus module's, on `cavalry:ipc`, from its start |
| service snapshot replay | 109 | the member's driver's, one for each start of a service from a snapshot: a replay of the snapshot's recording, whole, on `cavalry:ipc` ("Snapshots", "A service's start"; ADR 0132). A stream of its own, subscribed to only while a snapshot is read: on the service log stream a service that is not reading would hold the replay at its own position, as it holds another service's | the starting service's, on `cavalry:ipc`, while it reads its snapshot |

The members' ingress endpoints, listed as a redirect lists them ("Session event,
template 2"), `<id>=<host:port>` for each member separated by `,`, are at most 1,320
bytes: what one frame of the egress stream carries as a session event's detail. A
member does not start on a members string that lists more, and says to name the
hosts by address or by shorter names (ADR 0116).

A member sends a consensus message to one peer on its publication to that peer, and to
every peer by sending it on each. Every stream of this page carries messages of this
page's encoding and nothing else; a message on the wrong stream is rejected as
"What a reader rejects" says a template id the stream does not carry is.

## Encoding

Every message is one message of its stream, a message as `spec/log-buffer.md` claims
and `spec/frame.md` frames it, so that a reader is given it whole, fragments
reassembled. Multi-byte integers are little-endian; offsets are from the start of the
message. The first 8 bytes are the message header:

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 2 | block length | u16 | the length of the fixed fields that follow the header, the template's constant below |
| 2 | 2 | template id | u16 | which message, the ids below |
| 4 | 2 | schema id | u16 | 1, this page's schema |
| 6 | 2 | version | u16 | 0, the version of this page's layouts: every version of this page whose major is 0 writes 0, since none of them changes a layout ("Versions and the upgrade") |

The fixed fields follow at offset 8, in the order each table gives, every field
present; then the variable fields in the order each table gives, each a 4-byte `u32`
length followed by that many bytes, with no padding between or after; an ASCII field's
bytes are printable ASCII, 0x20 to 0x7E, and a data field's bytes are any. A session
message header is the one message whose fixed fields are followed by a payload with no
length of its own: the rest of the message is the payload, 0 or more bytes. The
longest message is the stream's (`spec/log-buffer.md`, "Claim, commit and abort"), and a message
longer than the MTU length less 32 crosses the wire as fragments that the reader
reassembles before this page sees it.

An id or a position that is none is −1, Aeron's null value, and each table says where
one is allowed: a member id is 0 or more except where its table allows −1, a position
is 0 or more except where its table allows −1, and a term id is −1 or more. A cluster
session id, a correlation id, a recording id, a type id and a log session id are any
value of their type, bounded by nothing on this page, the log session id being the
random session id of `spec/client-protocol.md`; a reader checks member ids, positions
and term ids and no other id ("What a reader rejects"). A `protocol version` field,
and a `version` field of a message's block, holds its writer's version of this page
in semantic form, major × 65536 + minor × 256 + patch: 256 for version 1, which is
0.1.0, and 0 for version 0, this page's version being the minor while the major is
0; the major is the value shifted right by 16 bits, arithmetic, so that a negative
value has a negative major; a reader rejects a consensus message whose major it does
not know, reads one of its own major whatever its minor and its patch ("Versions and
the upgrade"), and reads a session connect's whatever its major, so that the leader
can answer it ("Sessions"). A `bool` is an `i32`, 0 false and 1 true. An enumeration is an
`i32` with the values its table names and no other; a `flags` field is read bit by
bit, each table naming its bits, and a bit it does not name is ignored.

## The log

The log is one stream of entries, each one message, written by the leader alone. Its
position is the stream's (`spec/log-buffer.md`, "Positions"): an entry's position is
the position after the last frame that carries it, the position at which it begins
plus the aligned length of each of its frames, 32 plus the fragment's payload rounded
up to a multiple of 32, with a message longer than MTU length − 32 bytes split into
fragments of that payload length and a last of the remainder. A member's log, in this
version, is its recordings of stream 100 taken in position order: one per image it
joined live of a term's publication, with that term's session id, which is one per
term unless the member left the log within the term and joined it again ("Roles and
the election", state 0), a recording of an image with any other session id on its
`log` endpoint, which its election was never told, being no part of its log (ADR
0093), and one per
span it received from a replay ("Roles and the election", states 9 and 13), with the
replay's session id, since a recording holds one session id (`spec/durable-log.md`)
and each term's publication and each replay has its own. A member's append position
is the end of the contiguous span its recordings hold from the log's start, position
0: a position its recordings do not hold is a hole, and the append position stops at
the first hole whatever lies beyond it. A recording holds a position once a sync that
returned success has covered it: what counts is the recording's synced position,
`recording.synced_position` (`spec/client-protocol.md`, "Commands"), never what the
recorder has written and not yet synced, so that a position a member reports is one a
power loss leaves in its log (ADR 0060). The recorder syncs a recording by itself
once a second, the recording sync interval. So a member asks its driver to sync its
recording of the log, `sync recording`, whenever the recorder has written past what
it has synced, once for each position written that it sees and without waiting for
the answer: its append position then follows what it has recorded by one sync, and a
commit waits for one sync on a quorum (ADR 0069). A replay of the log takes each position from
one recording, the one that begins first among those that hold it, and moves to the
next recording at the first position the current one lacks; where two recordings
overlap, which state 13 makes, the positions they share are read from the earlier
one alone. Every member's log holds every entry at that position with the same
payload, since the leader's publication, every follower's image and every replay
carry one stream's entries, and a replay of any member's log from any position gives
the same entries as the leader's; the frame headers of a span differ between members
in their session id alone, until a replay can carry the log's session id. An image's join position is
the position its subscriber's position starts at when `available image` arrives
(`spec/client-protocol.md`, "Events"), the position of the first frame the image
holds; a member that records an image from its join position holds every position
from there.

A leadership term is the span of the log one leader writes. Its leadership term id is
an `i64` from 0 upward, one more than the term before it, never reused; −1 is no term.
The term base log position is the position at which the term's first entry, its new
leadership term event, begins. The log leadership term id of a member is the highest
term in its record of recorded terms, −1 for none, which is not the term of its last
entry. A member records a term, with its term base log position, at these points and
no other: as the leader, on entering leader log replication, state 5, with its append
position as the base; as a follower, when it takes the sender of a new leadership
term as its leader and its append position, after any truncation, is at the leader's
term base log position, before it reports that position ("Messages during an
election"); when a replication ends, state 9, the term replicated at its base and,
when the stop position is the leader's term base log position, the leader's term at
that base after it, both before it reports the stop; when it reads a term's new
leadership term event in a replay, state 10 or 13; and when it joins the live log,
state 15, the leader's term at the leader's term base log position when the record lacks it, which no path of this version into that state leaves lacking. So a
member that joined a term before its first entry reached it votes as a member of that
term, an election begins with the highest term in the member's record, and a member whose
log reaches the leader's term base counts toward the leader's commits. The record of the terms a
member has recorded, each with its term base log position, is kept in its durable
log directory and written before the member reports an append position for the
term, so that it survives a restart; its format is the replicated log's (card 227),
the fourth dependency this page names ("Members and streams"; ADR 0058). A member's
candidate term, the highest term it has voted in, proposed or followed, is kept beside
that record and written before the vote or the request vote that raises it is sent,
Aeron's node state file, so that a member votes once in a term across a restart; a
raise on following a leader is kept in memory alone, since a member counts toward a
term only once it has recorded that term, after which init takes the term from its
record ("Messages during an election"). Which recordings hold a member's log is
kept there too, in the log recordings file of `spec/durable-log.md`, and a member
reports no position a recording holds before a list that names the recording is
synced, so that a member that starts again finds every recording it reported a
position from (ADR 0108). A member reads all three when it
starts, in its first init and before it reads any message: its candidate term is the
higher of the stored one and the highest term in its record, its log leadership term
id is that highest term, and its append position is where the recordings that file
names end ("A member's start"). It
keeps nothing else across a restart: it follows no term and no leader, and knows a
commit position of 0, until a leader tells it (state 0; ADR 0089). A member's
append position is the end
of the contiguous span its recordings hold, as "The log" defines it, never a
recording's position beyond a hole; the commit position is the position the leader has
found a quorum has appended ("Roles and the election"). An entry at or below the
commit position is committed and never changes; an entry above it may be truncated by
the next leader, and the service reads nothing above it ("The service").

The entries, each a template of this page:

| Template | Entry | Who appends it | When |
| --- | --- | --- | --- |
| 1 | session message | the leader | a session message arrived from an open session on the ingress stream; the header is rewritten with the leader's term and timestamp, the payload is the client's; or the leader's own process appended one of no session, cluster session id −1 ("The leader's own entries") |
| 20 | timer event | the leader | a timer that the service beside the leader scheduled fell due on the leader's clock ("The service", "Timers"); a leader of versions 0 and 1 appends none, and a reader of every version accepts it |
| 21 | session open event | the leader | a session connect was accepted ("Sessions") |
| 22 | session close event | the leader | a session closed, with its reason |
| 23 | cluster action | the leader | the cluster takes a snapshot ("Snapshots"); the two other actions, suspend and resume of the ingress, are out of this version: no leader of it appends one, and a reader accepts each and does nothing with it |
| 24 | new leadership term event | the new leader | the first entry of every term |
| 25 | membership change event | the leader | a member joined or quit; out of this version, since membership is the members string, so no leader of this version appends it, and a reader accepts it |

### Session message header, template 1

Block length 24. On the log stream and the egress stream; on the ingress stream the
client writes it ("The session protocol").

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term the message belongs to |
| 16 | 8 | cluster session id | i64 | the session's, 0 or more; −1 on the log stream for a message of no session, which the leader's own process appended ("The leader's own entries") |
| 24 | 8 | timestamp | i64 | the cluster's time, milliseconds since 1970-01-01T00:00:00Z, the leader's wall clock when it appended the entry; 0 on the ingress stream, where the client writes it and the leader replaces it |
| 32 | rest | payload | bytes | the service's message, 0 or more bytes |

### Timer event, template 20

Block length 24, no variable fields.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term |
| 16 | 8 | correlation id | i64 | the timer's, any value: what the program that scheduled it chose |
| 24 | 8 | timestamp | i64 | the cluster's time, the leader's wall clock when it appended the entry: at or after the deadline the leader held for the timer |

### Session open event, template 21

Block length 36; then the response channel (ASCII) and the encoded principal (data,
empty in this version).

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term |
| 16 | 8 | correlation id | i64 | the session connect's |
| 24 | 8 | cluster session id | i64 | the id the leader gave the session: one more than the last it opened, 0 for the first of the cluster, recovered from the log by a new leader |
| 32 | 8 | timestamp | i64 | the cluster's time |
| 40 | 4 | response stream id | i32 | the client's egress stream id, 102 |

### Session close event, template 22

Block length 28, no variable fields.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term |
| 16 | 8 | cluster session id | i64 | the session's |
| 24 | 8 | timestamp | i64 | the cluster's time |
| 32 | 4 | close reason | i32 | 0 client action, the client's session close; 1 service action, out of this version and appended by no leader of it; 2 timeout, the session timeout passed ("Sessions") |

### Cluster action, template 23

Block length 32, no variable fields.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term |
| 16 | 8 | log position | i64 | the position at which the action takes effect, the entry's own |
| 24 | 8 | timestamp | i64 | the cluster's time |
| 32 | 4 | action | i32 | 0 suspend, 1 resume, 2 snapshot |
| 36 | 4 | flags | i32 | written 0 in this version; Aeron's, reserved; a reader ignores every bit |

### New leadership term event, template 24

Block length 48, no variable fields. The first entry of every term, so that a member
replaying its recording knows every term's base position and leader.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the new term |
| 16 | 8 | log position | i64 | the position at which the entry begins, the term base log position |
| 24 | 8 | timestamp | i64 | the cluster's time |
| 32 | 8 | term base log position | i64 | the same position |
| 40 | 4 | leader member id | i32 | the leader's |
| 44 | 4 | log session id | i32 | the session id of the leader's log publication for the term |
| 48 | 4 | time unit | i32 | 0, milliseconds, the one unit of this version |
| 52 | 4 | app version | i32 | the service's version in semantic form; 0 when the service names none |

### Membership change event, template 25

Block length 40; then the cluster members (ASCII), the members string after the change.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term |
| 16 | 8 | log position | i64 | the entry's position |
| 24 | 8 | timestamp | i64 | the cluster's time |
| 32 | 4 | leader member id | i32 | the leader's |
| 36 | 4 | cluster size | i32 | the members after the change |
| 40 | 4 | change type | i32 | 0 join, 1 quit |
| 44 | 4 | member id | i32 | the member that joined or quit |

## The consensus messages

Between members, on the consensus stream. A member reads a message from a member id it
does not have in the members string and ignores it; a message whose term is below the
reader's is answered or ignored as "Roles and the election" says.

### Canvass position, template 50

Block length 32, no variable fields. A member's report of its log to every other, while
it has no leader.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | log leadership term id | i64 | the highest term in the sender's record of recorded terms ("The log"), −1 for none |
| 16 | 8 | log position | i64 | the sender's append position |
| 24 | 8 | leadership term id | i64 | the sender's candidate term, the highest term it has voted in, proposed or followed ("The log"), at least its log leadership term id; −1 for none |
| 32 | 4 | follower member id | i32 | the sender's |
| 36 | 4 | protocol version | i32 | the sender's version in semantic form, 256; 0 from a member of version 0 |

### Request vote, template 51

Block length 32, no variable fields. A candidate's request to every other member.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | log leadership term id | i64 | the candidate's, as canvass position's |
| 16 | 8 | log position | i64 | the candidate's append position |
| 24 | 8 | candidate term id | i64 | the term the candidate proposes to lead, above every term it has known |
| 32 | 4 | candidate member id | i32 | the candidate's |
| 36 | 4 | protocol version | i32 | the sender's version in semantic form, 256; 0 from a member of version 0 |

### Vote, template 52

Block length 36, no variable fields. A member's answer to the candidate alone.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | candidate term id | i64 | the request's |
| 16 | 8 | log leadership term id | i64 | the voter's, as canvass position's |
| 24 | 8 | log position | i64 | the voter's append position |
| 32 | 4 | candidate member id | i32 | the request's |
| 36 | 4 | follower member id | i32 | the voter's |
| 40 | 4 | vote | bool | 1 for the candidate, 0 against |

### New leadership term, template 53

Block length 96, no variable fields. The leader's announcement to every other member,
repeated every leader heartbeat interval until the election closes, and to one member
that reports a term behind the leader's.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | log leadership term id | i64 | the term this message describes: the highest term in the leader's record at or below the term the receiver reported, which is the reported term when the leader's record holds it, and −1 when the record holds none at or below it, so that a leader answers for no term its own record lacks (ADR 0096), the `answered-term` vectors pinning it |
| 16 | 8 | next leadership term id | i64 | the lowest term in the leader's record above the log leadership term id, the leader's own term when none lies between, so that a reader whose log holds an uncommitted tail of the reported term truncates it to the next term's base, a departure from Aeron's lookup of the term one above, whose record has no gaps (ADR 0058), the `next-fields` vectors pinning it; −1 when the log leadership term id is the leader's own term |
| 24 | 8 | next term base log position | i64 | that term's base position, which is the leader's term base log position when the next term is the leader's own; −1 when the next leadership term id is −1 |
| 32 | 8 | next log position | i64 | that term's end position, the base position of the term after it in the leader's record; −1 when the next term is the leader's own, whose end is not known, or the next leadership term id is −1 |
| 40 | 8 | leadership term id | i64 | the leader's term |
| 48 | 8 | term base log position | i64 | the position at which the leader's term began, or the leader's append position before its first entry |
| 56 | 8 | log position | i64 | the leader's append position |
| 64 | 8 | leader recording id | i64 | the recording id of the leader's recording of its current term's log (`spec/durable-log.md`); −1 for none |
| 72 | 8 | timestamp | i64 | the cluster's time |
| 80 | 4 | leader member id | i32 | the leader's |
| 84 | 4 | log session id | i32 | the session id of the leader's log publication |
| 88 | 4 | app version | i32 | the service's version in semantic form; 0 for none |
| 92 | 4 | is startup | bool | 1 when the term is of the leader's first election since the leader started, 0 when the leader had closed an election before the one it leads this term from (ADR 0089) |
| 96 | 8 | commit position | i64 | the leader's commit position |

### Append position, template 54

Block length 21, no variable fields. A follower's report to the leader, on every change
of its append position and at least every leader heartbeat interval.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the sender's log leadership term id, the highest term in its record, as canvass position's |
| 16 | 8 | log position | i64 | the follower's append position |
| 24 | 4 | follower member id | i32 | the follower's |
| 28 | 1 | flags | u8 | bit 0 (`0x01`) catch-up: the follower is in follower catch-up (state 13) and on the live log, its append position at or past the position its live image joined at, so that the leader may stop its catch-up replay (ADR 0088); bit 1 (`0x02`) floor, from version 3: the follower holds a vote floor and has taken no floor answer for the term it follows, in this run, and asks its leader for one ("A member that lost its log"; ADR 0163), which a reader of an earlier version ignores; the other bits reserved, written 0, ignored |

### Commit position, template 55

Block length 20, no variable fields. The leader's report to every follower, on every
rise of the commit position and at least every leader heartbeat interval, the leader's
heartbeat.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the leader's term |
| 16 | 8 | log position | i64 | the commit position |
| 24 | 4 | leader member id | i32 | the leader's |

### Catch-up position, template 56

Block length 20; then the catch-up endpoint (ASCII), the follower's `catchup` endpoint
as `host:port`. A follower's request to the leader for the log from a position. A
leader of this version does not read the endpoint: its replay goes to the `catchup`
endpoint its own members string gives the follower, which is the same where every
member was started with one string (ADR 0085).

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the leader's term |
| 16 | 8 | log position | i64 | the position the follower wants the log from, its append position |
| 24 | 4 | follower member id | i32 | the follower's |

### Stop catch-up, template 57

Block length 12, no variable fields. The leader's word to one follower that its
catch-up replay stops: the answer to an append position with the catch-up flag.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the leader's term |
| 16 | 4 | follower member id | i32 | the follower's |

### Floor answer, template 58

Block length 28, no variable fields. From version 3: the leader's answer to an append
position with the floor flag, sent to that follower alone ("A member that lost its
log"; ADR 0163). No member of an earlier version sets the flag, so none is sent one.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the leader's term |
| 16 | 8 | log position | i64 | the leader's append position, read as it writes the answer and not as an earlier read left it ("A member that lost its log") |
| 24 | 4 | leader member id | i32 | the leader's |
| 28 | 4 | follower member id | i32 | the follower's, as the append position named it |
| 32 | 4 | session id | i32 | the session id of the data frame the append position came in (`spec/frame.md`): the session of the follower's consensus publication to the leader |

## The session protocol

Between a client and the leader: the client on the ingress stream to a member's
`ingress` endpoint, the leader on the egress stream to the client's response channel.
Template ids 7 and 8, Aeron's challenge and challenge response, are reserved in this
version and rejected ("What a reader rejects").

### Session connect, template 3

Block length 16; then the response channel (ASCII), a `cavalry:udp?endpoint=<host:port>`
channel the client subscribes to on the egress stream; the encoded credentials (data,
empty in this version); the client info (ASCII, any text the client gives, for an
operator's eyes, empty allowed).

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | correlation id | i64 | the client's, echoed by the answering session event |
| 16 | 4 | response stream id | i32 | 102, the egress stream; a connect naming another is refused ("Sessions") |
| 20 | 4 | version | i32 | the client's protocol version in semantic form, 256, and 0 from a client of version 0; a reader reads any major here, and the leader refuses one not its own ("Sessions") |

### Session event, template 2

Block length 44; then the detail (ASCII): empty for ok; the reason for error and
closed; the ingress endpoints for redirect, `<id>=<host:port>` for every member in the
members string's order, separated by `,`.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | cluster session id | i64 | the session's; −1 when none was opened |
| 16 | 8 | correlation id | i64 | the session connect's, or −1 for an event the client did not ask for |
| 24 | 8 | leadership term id | i64 | the sender's term |
| 32 | 4 | leader member id | i32 | the leader's, −1 when the sender knows none |
| 36 | 4 | code | i32 | 0 ok, 1 error, 2 redirect, 3 authentication rejected (reserved, sent by no member of this version), 4 closed |
| 40 | 4 | version | i32 | the protocol version of the member that sent the event, in semantic form: 256, and 0 from a member of version 0 |
| 44 | 8 | leader heartbeat timeout ns | i64 | 10,000,000,000, the leader heartbeat timeout in nanoseconds, so that a client knows how long a silent leader may be |

### Session close, template 4

Block length 16, no variable fields.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term the client knows |
| 16 | 8 | cluster session id | i64 | the session's |

### Session keepalive, template 5

Block length 16, no variable fields; the fields of session close.

### New leader event, template 6

Block length 20; then the ingress endpoints (ASCII), as session event's redirect detail.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the new term |
| 16 | 8 | cluster session id | i64 | the session's |
| 24 | 4 | leader member id | i32 | the new leader's |

### Session message header, template 1

On the ingress stream, written by the client with its term and session id, the
timestamp 0 and the payload its message; on the egress stream, written by the leader
with the term, the session id, the cluster's time and the service's reply.

## Snapshots

A snapshot is the state of a member's consensus module, or of the service beside it,
as it is at one position of the log, kept so that a start need read the log after
that position only. It is no stream of the cluster. The consensus module's is a
recording that the member writes through its own driver, on `cavalry:ipc` and the
snapshot stream ("Values"), and lists in the snapshots file of its log directory
(`spec/durable-log.md`, "Snapshots"; ADR 0128, ADR 0129). A service's is a recording
that the service writes through the same driver, on `cavalry:ipc` and the service
snapshot stream, and that the member lists for it in the same file ("A service's
snapshot", below; ADR 0131). A reader given a snapshot's messages reads them as the
`snapshot` stream of the vectors. A member that starts takes its state from the
newest snapshot of its consensus module that it can read, and reads its log from
that snapshot's position ("A member's start"; ADR 0130). A service whose program
asks it to starts from the newest snapshot of a service that its member lists, and
reads the log from that snapshot's position ("A service's start", below; ADR 0132).

**The action.** The leader asks for a snapshot by appending a cluster action whose
action is snapshot ("Cluster action, template 23"), an entry of its own as a session
open event is: with its term, the cluster's time and, as its log position, the
position at which the entry begins. It appends one in three cases. The first is by
itself: its log publication's position is the snapshot interval ("Values") or more
past the position after the last such action in its log, 0 where there is none, and
it serves sessions, its election closed and its log read to its term base log position
("Sessions"). The second is when its own process asks, once its election has closed
("The leader's own entries"). The third is when a command on its host asks, with a
control toggle ("The control toggle", below). An action that the back-pressured log
does not take is tried again on a later duty cycle in the first case, is refused to
the process that asked in the second, and is tried again at the next look in the third.

**The control toggle.** A command on a member's host asks that member for a snapshot
without a message: it adds a counter, `cluster.control_toggle`, through the member's
driver (`spec/client-protocol.md`, "Commands", add counter), and stores 4 in it, which
is snapshot. The counter's key is the member's id and then the toggle's id, the
command's client id, a number no other client of that driver has
(`spec/client-protocol.md`, "The driver directory"): 12 bytes, an i32 and an i64,
little-endian. The command is the counter's one writer, as every counter has one
(`spec/counters-file.md`). The counter is gone when the command removes it, and a
command that ends without removing it has it freed by the driver, which forgets a
client at its close or once it has timed it out (`spec/client-protocol.md`,
"Liveness and timeouts"). The id is the adder's word: a leader takes it as the key
gives it. In Aeron, this is the cluster control toggle, which the consensus module
allocates and a tool writes; here the command that asks allocates it, since a second
writer of one counter is what the counters file does not have.

A leader that serves sessions looks for a toggle of its own id in its driver's
counters file: at most the records of the control toggle look ("Values") at a time,
in a ring over the file's metadata records, and no more often than that look's
interval. A member that does not lead looks for none and takes none. The leader takes
a toggle that holds 4, and whose id is above that of the last toggle it took, when it
takes no snapshot of its own and its read of its own committed log ("Sessions") has
passed the last cluster action it appended, where its log still holds that action:
it appends the action, stores the snapshot's log position, the position after the
action's entry, in `cluster.control_toggle_snapshot_position`, and then the toggle's
id in `cluster.control_toggle_taken`, each with a release store. A toggle it cannot
take yet waits where it is. A toggle that holds another value is no request of this
version, and the leader passes it over.

A leader takes each toggle once: it takes none whose id is at or below the id of the
last it took. A driver gives its clients rising ids, since a client's id comes from
the counter of the driver's command ring (`spec/client-protocol.md`, "The driver
directory"). So a toggle that stays, because its command has not ended or never
will, is not taken again and holds no later toggle back, and a member that stops
leading and leads again while a toggle is still there does not take it twice: one
toggle is at most one cluster action. Of two commands that ask one member at once,
each has its toggle taken, with a snapshot of its own, when the leader's look meets
the toggle with the lower id first. When it meets the higher first, the lower is
never taken, and the snapshot of the higher answers both commands, as the next
paragraph says. A toggle keyed with an id that the driver has given no client yet
would leave every toggle below it untaken until the member starts again; a command
writes its own client id and no other.

The command that asked reads the answer in the member's counters, and what answers
it is a snapshot that the member asked for after the command began. Before it adds
its toggle the command loads the member's `cluster.control_toggle_taken`. A member
of a build before this one has no such counter and serves no toggle, and the
command ends at once and says so. The id it loads is below the command's own, or
the command takes a new client id, by attaching again, and loads it again. Then it
adds its toggle and waits for `cluster.control_toggle_taken` to hold an id at or
above its own: the leader took a toggle after that first load, the command's own or
a later one, so the cluster action was appended after the command began. It loads
that id with an acquire load and then `cluster.control_toggle_snapshot_position`, as
a reader that branches on a position does (`spec/counters-file.md`, "Value slot"):
the member's release store of the id comes after its store of the position, so the
position the command reads is that toggle's or a later one's, on a machine that
orders stores weakly as on any other. The command keeps that position, and its
request is answered when `cluster.snapshot_position` is at or past it: the member
lists a snapshot of a position that its log reached after the command began. A
snapshot that another action asked for before the command began does not answer it,
since its position is below every position stored after.

The command waits for no longer than the control toggle timeout ("Values"), and
removes its toggle when it ends, however it ends. A member that stops leading while
a toggle waits leaves it: the command reads `cluster.role` while it waits, and ends
when the member no longer leads. A member that gives the snapshot up
("A snapshot that fails") lists none at the position, and the command's wait ends at
its timeout. A leader that is closing may append the action and store neither
gauge. A follower that is still taking the snapshot before it when it reads a
toggle's action gives the later one up, counted in its `cluster.snapshots_failed`,
as for any two actions close together: the leader waits for its own snapshot and
cannot see a follower's. The id a member took last is its process's own: a member
that starts again and leads takes a toggle it finds, whatever its process took
before, which is at most one more snapshot.

**The snapshot's position.** Every member takes the snapshot when its consensus
module's read of its own committed log ("Sessions") reaches the action. The action is
then committed, and the member has applied every entry before it. The read ends at
the action's entry in that duty cycle. The snapshot's log position is the position
after the action's entry. Its leadership term id and its timestamp are the entry's,
and its term base log position is that term's in the member's record of terms. Every
field a member writes is a function of the committed log below that position, and
nothing a leader holds of its own, a connect it has accepted and not yet seen
committed among them, is written. So the snapshots that the members take for one
action are of one position and of one state, and they hold the same bytes.

**What a member writes.** A member holds one exclusive publication on the snapshot
stream from its start to its close, and each snapshot is a recording of a span of
that publication's stream. In the duty cycle that read the action the member starts
a recording of the publication (`spec/client-protocol.md`, "Commands", start
recording), which begins at the publication's position. The recording must be of
that publication's session. The member then writes these messages, each as one
message of the publication: a snapshot marker whose mark is begin; a cluster session
for each open session, in the order of the sessions' ids; a consensus module; and a
snapshot marker whose mark is end. Both markers carry type id 1, the snapshot's log
position and leadership term id, index 0, time unit 0 and app version 0. While the
messages are being written the member reads no entry of its committed log, so that
the sessions it writes are the sessions at the snapshot's position. That is one duty
cycle unless the publication is back-pressured. When the recorder has written to the
end of the last message, the member stops the recording (`stop recording`), which the
driver answers once the catalog's stop is written and synced. When the driver has
announced the stop, the recording stopped at the end of what the member wrote, and
the member's append position, what its own log has synced ("The log"), has reached
the snapshot's log position, the member lists the snapshot in its snapshots file
with service id −1. The snapshot counts from that write on. The last condition holds
the snapshot to a position that the member's log keeps through a crash of any kind,
a power loss included, since the driver's recovery keeps what a recording synced
(`spec/durable-log.md`, "The driver"); a start refuses a log that ends below its
snapshot ("A member's start"; ADR 0143). The member read the action from its own
log, and it asks its driver to sync that log whenever the recorder has written past
what it synced ("The log"), so the wait is that one sync, already asked; the bound
on it is the snapshot log sync wait ("Values").

**A snapshot that fails.** Each of these ends the snapshot: a command that the driver
refuses; a recording of another session; a message that the publication does not take
for the snapshot timeout ("Values"); a stop that is not announced within that
timeout; a recording that stopped short of what was written; a log that has not
synced to the snapshot's log position within the snapshot log sync wait ("Values")
after the stop was read; and a cluster action read while a
snapshot is being taken, which ends the later one. The member asks the
stop of a recording that has not stopped, lists nothing, counts the snapshot in
`cluster.snapshots_failed` and goes on: its log is whole, and the next action asks
again. A recording left behind is in no list and is no snapshot, and what was
written of it lies in the publication's stream below where the next snapshot's
recording begins. A list that cannot be written ends the member's duty
cycle with that error, as a list of its log's recordings does. A member takes no
snapshot for an action whose position its newest listed snapshot is at or past. A
member that started from an older snapshot than its newest listed one, or from
none, meets such actions in its log.

**A service's snapshot.** A service's read of its member's committed log ("The
service") ends at a cluster action whose action is snapshot, once its program has
taken that entry: the entry is the last one of that read. The program's state is
then its state at the snapshot's log position, the position after the action's
entry, and it is the same state beside every member. Before the service reads the
next entry its program may give the service that state, as bytes that mean
something to the program alone, at most the service snapshot state of "Values". A
longer state is refused before anything is written: the service tells its program,
counts nothing, and the snapshot stays due until the next read. A
program that gives none and reads on takes no snapshot for that action. A service
whose member already lists a service's snapshot at or past that position, which
`cluster.service_snapshot_position` holds, writes none and tells its program so.

Given the state, the service writes a recording through its member's driver. It
holds one exclusive publication on `cavalry:ipc` and the service snapshot stream
from its first snapshot to its close, and each snapshot is a recording of a span
of that publication's stream. It starts a recording of the publication, which must
be of that publication's session, and writes these messages, each as one message
of the publication: a snapshot marker whose mark is begin; a client session for
each session that is open at the snapshot's position, in the order of the
sessions' ids; a timer for each timer that is pending at that position, in the
rising order of the timers' correlation ids ("The service", "Timers"); a snapshot
marker whose mark is section, with index 1; the
program's bytes in their order, 1,024 to a message and the rest in the last; and
a snapshot marker whose mark is end. The three markers carry type id 2, the
snapshot's log position, the leadership term id of the action's entry, time unit 0
and app version 0, and the begin and end markers index 0. After the section marker
every message but the recording's last is the program's bytes, whatever they hold:
a reader takes them by their place and does not read them as messages of this
page. A state of no bytes has no message there. A snapshot at whose position no
timer is pending has no timer, and is byte for byte the snapshot that a service of
version 1 writes.

When the recorder has written to the end of the last message the service stops the
recording, and the recording must have stopped at the end of what the service
wrote. The service then sends its member a service ack on the consensus module
stream, with the snapshot's log position, the timestamp of the action's entry, the
recording's id as its relevant id, and service id 0. Each of these ends the
snapshot: a command that the driver refuses; a recording of another session; a
message that a publication does not take for the snapshot timeout; a stop that is
not announced within that timeout; a recording that stopped short of what was
written; and an ack that the consensus module stream does not take within that
timeout, which is a member that is not running. The service asks the stop of a
recording that has not stopped, reports the failure to its program, counts it in
`cluster.service_snapshots_failed`, and reads on when its program next reads. A
service adds its counters ("Counters", the ones a service adds) before its
publications, when it first opens its member's log ("The service", "Timers",
"What a service tells its member"; ADR 0168), so that a
publication the driver refuses at a snapshot is counted, as that snapshot's
failure. Two failures are counted in no counter of the service's: a counter that
the driver refuses, for which there is no counter yet, and the publication on the
consensus module stream when the driver refuses it at a read, where no snapshot
fails. The service reports each to its program, as the error of that snapshot or
that read, and its next snapshot, or its next read, adds what is missing. The
driver counts every command it refuses (`client_protocol.errors`; ADR 0159). A
snapshot whose ack the stream took is counted in `cluster.service_snapshots_taken`.

**The member's list of a service's snapshots.** A member subscribes to the consensus
module stream on `cavalry:ipc` from its start and reads it in every duty cycle, at
most 10 messages. For a service ack it lists the snapshot in its snapshots file,
with the ack's service id, when four things hold. The service id is 0: one service
beside a member takes snapshots in this version. The catalog of the member's
driver holds the recording that the relevant id names, of the service snapshot
stream, stopped and with bytes. The log position is above 0 and at or below the
member's commit position. And the file takes the entry: its log position is a
multiple of 32 and above that of the newest snapshot the file lists for the
service id (`spec/durable-log.md`, "Snapshots"). The member does not check that an
entry of its log ends at the position: it takes the service's word for the
position its state is of. The entry's leadership term id and term base
log position are those of the newest term in the member's record of terms whose
term base log position is below the log position, and its timestamp is the ack's.
The member counts a snapshot it lists in `cluster.service_snapshots_listed` and
stores its log position in `cluster.service_snapshot_position`. An ack that fails
one of the four is counted in `cluster.service_acks_rejected`, and nothing is
listed. A list that cannot be written ends the member's duty cycle with that
error, as its own snapshot's list does.

A member answers no ack and waits for none. It does not know which services read
its log (ADR 0066), so its own snapshot counts without a service's, and a
service's counts without its own. An ack that the member does not read, because
it stopped, is a snapshot that is in no list: the next action asks again.

**A service's start.** A service that has read no entry starts from a snapshot when
its program asks it to, before its first read of the log. A program that does not
ask reads the log from position 0, as a program of a version before this did, and
needs its member's whole log ("A member's start"). The
member stores the recording id of the newest snapshot of a service that it lists
in `cluster.service_snapshot_recording_id`, −1 while it lists none, from its start
and from each listing on. The service loads that counter from a record that is the
member's by the test of "The service", and only of a member whose live recording
it has found: the member adds the counter and stores its value before it stores a
recording's id in `cluster.log_recording_id`, since a counter holds 0 between its
allocation and its first store, and 0 is a recording id. With no such member on
its driver yet it has nothing to start from, and its program asks again. With −1,
or a member that has no such counter, it reads the log from position 0.

Otherwise the service plays the
recording back whole, on `cavalry:ipc` and the service snapshot replay stream
("Members and streams"), reading its own replay's session, and takes the snapshot
when three things hold. The driver plays
the recording to its end, with no more than the service snapshot load timeout
("Values") before the first message and between two. The messages are a service's
snapshot: a snapshot marker whose mark is begin, with type id 2, index 0 and a log
position above 0; a client session for each session, at most 10, in the rising
order of their cluster session ids; a timer for each pending timer, at most a
service's timers of "Values", in the rising order of their correlation ids, no two
alike; a snapshot marker whose mark is section, with
index 1 and the begin marker's type id, log position and leadership term id; the
program's bytes, at most the service snapshot state of "Values"; and, as the
recording's last message, a snapshot marker whose mark is end, with index 0 and
the same type id, position and term. And the service's table of sessions takes
the sessions whole: a response channel longer than a session holds is one it does
not take. A snapshot that fails one of the three is passed over, counted in
`cluster.service_snapshots_passed_over`, and the service reads the log from
position 0. The member's newest is the one snapshot a service is told of.

The driver's refusal of the replay passes the snapshot over only when it is a
refusal for the recording, which a later start meets again: error code 21, a
recording the driver does not hold as a valid one, or code 23, no bytes of it where
the replay starts (`spec/client-protocol.md`, "Error codes"). Every other refusal
of a command of the start is no snapshot passed over: code 10, a registry, the
counters file or a queue at its bound; code 24, a retention command at work on the
recording, after which the recording plays or is unknown; code 12, which a segment
file that cannot be read and a replay's log that cannot be created share; any
other code; and any refusal of the add of the service's two counters of its start
or of its subscription. The service reports the refusal to its program with the
driver's code, has not started and has counted nothing, and its program asks
again.

From the snapshot the service takes its sessions, each open as its session open
event left it, its timers, each pending with its deadline, and the position its
read of the log begins at, the snapshot's log
position, which `cluster.service_snapshot_start_position` holds. It gives its
program the program's bytes as the program gave them. And it takes the snapshot's
leadership term id as the term of the last new leadership term event it read,
without that event's leader: it answers an entry of that term when its member's
counters say that the member leads the term ("Replies"), which only the member
that the term's event names says. An entry that its member held when the service
opened its log is history, as before, and the service answers none.

### Snapshot marker, template 100

Block length 40, no variable fields. The first and last message of a snapshot. In
a service's snapshot a marker whose mark is section stands before the program's
bytes.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | type id | i64 | whose snapshot: 1 the consensus module's, the member's own state; 2 a service's |
| 16 | 8 | log position | i64 | the position the snapshot is of, the position after the cluster action's entry: every entry below it is in the state |
| 24 | 8 | leadership term id | i64 | the term at that position |
| 32 | 4 | index | i32 | the section's index: 0 for the begin and end markers, 1 for the section marker of a service's snapshot |
| 36 | 4 | mark | i32 | 0 begin, 1 section, 2 end |
| 40 | 4 | time unit | i32 | 0, milliseconds |
| 44 | 4 | app version | i32 | the service's version in semantic form, 0 for none |

### Cluster session, template 103

Block length 40; then the response channel (ASCII). One open session of the member,
in a consensus module's snapshot.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | cluster session id | i64 | the session's |
| 16 | 8 | correlation id | i64 | the correlation id of the session connect that opened it |
| 24 | 8 | opened log position | i64 | the position at which the session's session open event begins, 0 or more |
| 32 | 8 | time of last activity | i64 | written −1, as Aeron writes it; a reader ignores it, since a member counts a session's timeout from when its leader begins to serve ("Sessions") |
| 40 | 4 | close reason | i32 | written −2147483648, the null of an enumeration of an `i32` in Aeron's encoding, since a session in a snapshot is open; a reader ignores it, and it is no enumeration of this page |
| 44 | 4 | response stream id | i32 | the client's egress stream id, 102 |

### Consensus module, template 105

Block length 28, no variable fields. The consensus module's own state, once in a
snapshot, after its sessions.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | next session id | i64 | one more than the highest cluster session id of a session open event below the snapshot's log position, 0 for none: the log's, and not a leader's own next session id, which it moves when it accepts a connect, before that session open event is committed ("Sessions") |
| 16 | 8 | next service session id | i64 | written 0; Aeron's, of the messages a service appends to its own cluster's log, which no service does here; a reader ignores it |
| 24 | 8 | log service session id | i64 | written 0; Aeron's, as the field before it; a reader ignores it |
| 32 | 4 | pending message capacity | i32 | written 0; Aeron's, as the two fields before it; a reader ignores it |

### Client session, template 102

Block length 12; then the response channel (ASCII) and the encoded principal (data).
One session that is open at the snapshot's position, in a service's snapshot.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | cluster session id | i64 | the session's |
| 16 | 4 | response stream id | i32 | the client's egress stream id, 102 |

The encoded principal is written with no bytes, since this version has no
authentication ("Out of this version"), and a reader ignores it.

### Timer, template 104

Block length 16, no variable fields. One timer that is pending at the snapshot's
position, in a service's snapshot ("The service", "Timers"). In Aeron it is in the
consensus module's snapshot, where the timers are the consensus module's; here they
are the service's (ADR 0151).

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | correlation id | i64 | the timer's |
| 16 | 8 | deadline | i64 | the cluster's time at or after which it is due |

### Service ack, template 33

Block length 36, no variable fields. On the consensus module stream, from a
service to the member on its driver: the service took a snapshot.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | log position | i64 | 0 or more: the log position of the service's snapshot |
| 16 | 8 | timestamp | i64 | the cluster's time of the cluster action that asked for it |
| 24 | 8 | ack id | i64 | written 0; Aeron's count of a service's acks, which a member here does not keep; a reader ignores it |
| 32 | 8 | relevant id | i64 | the id of the snapshot's recording in the catalog of the member's driver |
| 40 | 4 | service id | i32 | the service's id, 0 |

### Schedule timer, template 31

Block length 16, no variable fields. On the consensus module stream, from a
service to the member on its driver: the timer is pending in the service with
this deadline, said when its program schedules it and again while it is pending
("The service", "Timers", "What a service tells its member").

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | correlation id | i64 | the timer's, any value |
| 16 | 8 | deadline | i64 | the cluster's time at or after which the timer is due, any value: one in the past is due at once |

### Cancel timer, template 32

Block length 8, no variable fields. On the consensus module stream, from a service
to the member on its driver: the timer is pending no more.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | correlation id | i64 | the timer's |

## Roles and the election

A member is a follower, a candidate or the leader. The leader writes the log and
commits it; a follower records the log and reports its append position; a candidate
is a follower asking for votes. A cluster of n voters has a quorum of n ÷ 2 + 1,
integer division: 2 of 3, 3 of 5, 4 of 7, and 1 of 1. A standby is a follower that
is no voter, and where this section counts members it counts voters ("Standbys").

**The commit position.** The leader keeps each member's last append position, the
term it was reported for and the time it arrived, its own among them, set to its own
append position and term on every duty cycle. A member is active while its last
append position is younger than the leader heartbeat timeout: the position it last
reported, in an append position or in a canvass position, which carries its append
position too, so that either keeps it active (ADR 0083). A member counts toward
the quorum position only when it is active, its last report was for the leader's
term, and the position is at or past the leader's term base log position: a position
below the base, a canvass position for an older term and an append position for an
older term count for nothing, since a member whose highest recorded term is older may
vote for a log that lacks what the leader would commit, Raft's rule that a leader
commits no older term's entries by counting replicas, which departs from Aeron (ADR
0058). The quorum position is the quorum-th highest position among the members that
count, 0 when fewer than a quorum count; the commit position is the lesser of the
quorum position and the leader's own append position, and it never falls: during an
election it stays where it was until a quorum has recorded the leader's term, and
then it becomes the term base log position. The `commit-position` vectors pin the
count. The leader sends commit position to every follower when it rises and, as its
heartbeat, when the leader heartbeat interval has passed since the last. A follower
that has had no commit position or new leadership term from its leader for the leader
heartbeat timeout holds an election; a leader whose active members fall below a quorum
holds one too, counted in `cluster.leader_quorum_losses`. A follower whose image of its leader's
log is no longer available (`spec/client-protocol.md`, "Events", unavailable image)
holds one as well, counted in `cluster.log_images_lost`: it can append nothing more
from an image it does not have, and the image it joins after is checked where it
joins (state 15).

**Logs compared.** One log is at least another when its log leadership term id, the
highest term in its record, is higher, or equal with a log position at least the other's: the
term decides, then the position. A member votes for a candidate whose log is at least
its own and against one whose log is behind it; a member that joined a term and holds
none of its entries still outranks a longer log of an older term, which is what keeps
a committed entry from being truncated (the `compare-log` and `candidate` vectors).

**The states.** An election is the state machine Aeron's `ElectionState` names, one
state at a time per member, each with its code; an implementation stores the code in a
counter ("Counters").

| Code | State | What the member does, and what moves it on |
| --- | --- | --- |
| 0 | init | consolidates its log: stops any replay or catch-up and leaves the live log, removing its subscription on its `log` endpoint and so stopping its recording of the leader's image there, so that the image it joins next is a new one whose join position state 15 checks; takes the higher of its stored candidate term and the highest term in its record as its candidate term, both read from its log directory in the member's first init since it started and held in memory from then on ("The log"; ADR 0089); and forgets every member's report, so that the reports a member in canvass holds are the canvass positions it has read since its last init, or since it last left follower log replication, which forgets them too (state 9; ADR 0106). A cluster of one member goes to leader log replication as leader of candidate term + 1; every other member goes to canvass |
| 1 | canvass | as a follower, sends canvass position to every other member every election status interval; a member has reported once this member has read a canvass position from it since this member's last init or since it last left follower log replication (states 0 and 9), and its report is the last one read; moves to nominate at once when every member has reported and no member's log is ahead of its own, a unanimous candidate, or when the canvass deadline has passed and the members that have reported with a log not ahead of its own, itself among them, make a quorum, a quorum candidate ("Logs compared"; the arithmetic vectors pin both). The deadline is the startup canvass timeout after the state began until the member's first election since it started has closed, on an empty log directory or on one it kept, and the leader heartbeat timeout after the last word from a leader from then on (ADR 0081, ADR 0089). A member that owes a truncation moves to nominate in neither case until the truncation has finished ("Messages during an election"; ADR 0148). A member that receives a request vote for a term above its candidate term while canvassing votes for it, when the candidate's log is at least its own, and goes to follower ballot, as "Messages during an election" says; one that receives new leadership term for a term at least its candidate term goes to follower replay or follower log replication as that message's rules say |
| 2 | nominate | waits a random delay up to half the election timeout, sending canvass position every election status interval, then takes candidate term + 1 as its candidate term, stores it before it sends anything, records its vote for itself and goes to candidate ballot; while it waits it reads request vote and new leadership term as a member in canvass does, voting for a candidate and going to follower ballot, or following a leader, so that a member that learns of a leader during its delay opens no ballot against it (ADR 0081) |
| 3 | candidate ballot | as a candidate, sends request vote to every member it has not sent one to; becomes leader of the candidate term, going to leader log replication, at once when the votes for it for that term, its own among them, make a quorum and every member it has heard has cast one of them, or when the election timeout has passed and a quorum has voted for it, no member having voted against for that term in either case, a vote for an older candidate term of its own counting for nothing ("Messages during an election"); a member is heard once the candidate has read a consensus message from it since its election began at init, so that a member that has said nothing since then, a leader that was killed among them, is not waited for, a departure from Aeron, whose candidate waits the election timeout for every member but a leader that closed gracefully (ADR 0077); goes back to canvass at once when it can no longer win, a vote against for that term having been cast, since no ballot with one makes a leader, a second departure from Aeron, whose candidate waits out the election timeout with a vote against (ADR 0105), and it reads its ballot before it sends, so that in the duty cycle in which it goes back it sends no request vote, not one its publication had refused before, and asks no member for a vote it can no longer use; and goes back to canvass when the election timeout has passed and it has not become leader, short of a quorum. A request vote from another candidate for a higher term is answered as "Messages during an election" says: for, moving it to follower ballot, when that candidate's log is at least its own; against, taking the term, when that candidate's log is behind, after which it holds no vote for its candidate term, its own included, can lead nothing in this ballot, and goes back to canvass at once; the `ballot` vectors name no candidate, so they say only whether a vote against the candidate term has been cast, and that the candidate still holds its own vote is the member's to check. A member that goes back to canvass from a ballot, a candidate from this state or a voter from state 4, keeps the reports it holds, a third departure from Aeron, whose election forgets every member's report each time it enters canvass: the two candidates of a split vote can be candidates again without waiting an election status interval for each other's canvass position (ADR 0081, ADR 0106) |
| 4 | follower ballot | waits for the candidate it voted for; goes to follower replay or follower log replication on its new leadership term, or back to canvass when the election timeout passes |
| 5 | leader log replication | as the leader, records its term with its append position as its base, before its own position counts, then adds its log publication at that position, the term base log position, which needs a publication created at a position ("Members and streams"); sends new leadership term and commit position every leader heartbeat interval; serves the catch-up positions of followers replicating what they lack (state 9, "Messages outside an election"); and waits until the quorum position reaches its append position, then goes to leader replay. It fails the election when its active members, those whose last append position is younger than the leader heartbeat timeout, fall below a quorum, and begins again at init |
| 6 | leader replay | replays nothing in this version and goes to leader init at once. The consensus module's own state, the open sessions and their ids, comes from its read of its committed log, which runs in every state and no further than the commit position ("Sessions"; ADR 0116), so that no entry the next leader may truncate is ever in it; a service reads its member's recording itself, from a position of its own ("The service") |
| 7 | leader init | joins its log as leader for the term at its append position, the term recorded on entering state 5 ("The log"), and goes to leader ready |
| 8 | leader ready | sends new leadership term every leader heartbeat interval until a quorum of members counted as "The commit position" counts them, for the leader's term and at its log position, has reported within the leader heartbeat timeout, then appends the new leadership term event and closes the election. It fails the election when its active members fall below a quorum, as state 5 does, and begins again at init (ADR 0082) |
| 9 | follower log replication | replicates what it lacks of the term its append position lies in, one term at a time: a member whose append position is at the next term base log position holds none of the term it is to replicate, and first records that term at that base, as a member that joins a leader at its base records the leader's term there, so that a replication that fails midway leaves it a log of a term its record holds, which the next new leadership term carries on from its append position, where a term recorded only at the stop would leave it a truncation below a commit position it was told (ADR 0086); it records that term only while its append position is still that base, the new leadership term having been read with the append position of the duty cycle before: a member whose append position has risen above the base by the duty cycle that would record the term holds bytes above the base that are of no term it may record there, and records nothing, subscribes to nothing, asks for nothing and fails its election, counted in `cluster.elections_failed`; a stop position at its append position, a term with nothing in it, is reached at once, and nothing is subscribed to or asked for; otherwise it subscribes to stream 100 on its `catchup` endpoint, as state 11 does, with a recording armed on the subscription, and sends catch-up position with its append position and that endpoint, once from a position, tried again each duty cycle while its publication to the leader does not take it, counted in `cluster.log_replications` when it has left; the leader answers each catch-up position with one replay, of the recording of its own that holds the position, which ends at the stop position, the next term base log position when the append position is below it, else the next log position, or at that recording's end where it comes first ("Messages outside an election"; the `replication-stop` vectors); the follower records the replay, an image of stream 100 on its `catchup` endpoint with the replay's session id, whose join position must be the position it asked from, an image that joins elsewhere failing the election, counted in `cluster.join_position_mismatches`, reporting append position as it goes, on every change and at least every leader heartbeat interval; when the image has gone with its append position still below the stop position, the replay having ended at its recording's end or stopped, the follower removes the subscription, the stop of the image's recording being where its log then ends, and, when that is above the position it asked from, subscribes again, with a recording armed again, and asks again from its append position, each catch-up position counted, so that the image of every replay finds a recording armed for it: a driver arms a recording only on a stream that has no image, and starts one on an image already there at the position the image has reached (`spec/client-protocol.md`, "Commands", start recording; ADR 0092); a follower whose log ends where it asked from asks no more, and the first timer below fails its election. An append position above the stop position, a replay that carried it past the term's end, fails the election, counted in `cluster.elections_failed`. Two timers bound it, Aeron's: while its append position is below the stop position, an append position that has not moved for the leader heartbeat timeout fails the election, counted in `cluster.log_replications_unavailable`, the leader's recordings not holding the span, purged by retention, or a replay that stopped for a fault being the causes; once its append position has reached a stop position that is the leader's term base log position it waits for the commit position it was told to reach its append position, a commit position or a new leadership term from its leader extending that wait by the leader heartbeat timeout, and a wait that runs out fails the election, counted in `cluster.elections_failed`; a stop below the base has no such wait, since the leader's commit position cannot rise until a quorum has recorded its term, which this follower cannot do before the next term. When the stop position is reached the term replicated is in its record with its base, and when the stop is the leader's term base log position, however the stop was given, the next term base or the next log position, it records the leader's term at that base after it, before it reports the stop, the `replication-record` vectors pinning what its record then holds, so that it counts toward the leader's commits as a member that joined the term does; then it goes back to canvass, removing the subscription and holding no member's report, as a member that comes from init holds none: the reports it held were read against the log it had before, and against the log it has now they could make it a candidate with a leader in place, as in Aeron, whose election forgets every member's report each time it enters canvass (ADR 0106); a later new leadership term carries it on to the next term |
| 10 | follower replay | replays its own log, its recordings in position order, from its log position to the lesser of its append position and the commit position it was told, into no state of the consensus module's, whose sessions come from its read of its committed log (state 6; ADR 0116), and into no service ("The service"), recording each term whose new leadership term event it replays, the leader's term already recorded at the base by the new leadership term that sent it here ("The log"), then goes to follower catch-up init when its append position is behind the leader's log position, else to follower log init; a replay that ends short of its append position goes back to canvass |
| 11 | follower catch-up init | subscribes to the log stream on its `catchup` endpoint and sends catch-up position with that endpoint, tried each duty cycle until its publication to the leader takes it and counted in `cluster.catchups` when it has left (ADR 0088); goes to follower catch-up await once sent, and fails the election when it cannot send within the leader heartbeat timeout |
| 12 | follower catch-up await | waits for the image of the leader's catch-up replay, the one image of stream 100 on its `catchup` endpoint, whatever its session id, since a replay's publication has a session id of its own (`spec/client-protocol.md`, "Commands", start replay), checks that the image's join position ("The log") is its append position, and goes to follower catch-up when it has joined it; an image that joins elsewhere, a stale replay's among them, fails the election, counted in `cluster.join_position_mismatches`, and so does no image within the leader heartbeat timeout |
| 13 | follower catch-up | records the catch-up replay, recording each term whose new leadership term event it reads, reporting its append position, the contiguous end, and adds its `log` endpoint's live subscription once its append position is within the live-add window of the leader's position, the greater of the log position and the commit position its leader told it, since the commit position is the one a leader goes on sending (ADR 0088), the window being the lesser of term length ÷ 4 and 1,048,576 bytes; the live image's recording begins at the live recording's start, the position the follower joined the live log at, which lies at or past its append position, so that the two recordings overlap from there and the append position follows the replay until it reaches the live recording's start and the further of the two from then, the end of what the member's recordings hold ("The log"), which is the replay's recording for as long as the replay is ahead of the live log, so that a catch-up may end with the member's log ending above what its live recording has synced ("The service"; ADR 0135); its reports carry the catch-up flag from the one that first reports a position at or past the live recording's start, sent in that duty cycle, and none before, since the replay reads no further than the leader's recording has synced (`spec/client-protocol.md`, "Commands", start replay) and a replay stopped sooner would leave the positions below the live recording's start unfilled (ADR 0088); goes to follower log init once the leader has sent stop catch-up, which it sends for each report with the flag and on which the follower removes its catch-up subscription, and its append position has reached the live recording's start and the commit position it had been told when it read that stop catch-up, a position the leader's later commit positions do not raise, since under a steady load they would hold the follower in the state for as long as the load lasted (ADR 0088); fails the election when the catch-up makes no progress, its append position not having moved, for the leader heartbeat timeout |
| 14 | follower log init | subscribes to the log stream on its `log` endpoint when it has not, and goes to follower log await; one already subscribed goes to follower ready |
| 15 | follower log await | waits for the live image by the log session id and for its recording of that image to have started, checks that the image's join position, which is where that recording begins ("Members and streams"; ADR 0093), is its append position, joins it there, records the leader's term at the leader's term base log position when its record lacks the term, which no path of this version into this state leaves lacking ("The log"), and goes to follower ready; an image that joins elsewhere, the leader having appended past the follower's position while it subscribed, fails the election, counted in `cluster.join_position_mismatches`, so that the member canvasses again and catches up, and so does no image within the leader heartbeat timeout |
| 16 | follower ready | sends append position to the leader and closes the election; fails it when it cannot send within the leader heartbeat timeout |
| 17 | closed | the election is over: the leader leads the term, a follower follows it |

**How long an election takes.** When a leader is lost, each follower's leader
heartbeat timeout ends within one leader heartbeat interval of the others', and the
canvass deadline has passed for each as its election begins. Their canvass positions
cross within one election status interval, 100 ms; nominate waits at most half the
election timeout, 500 ms; and the ballot is the candidate's stored term, one request
vote, the voter's stored term and one vote for each member heard. So a member leads
the next term less than a second after the last of those timeouts wherever the two
synced writes and the round trip take under 400 ms together. An election is longer in
two cases. Two candidates whose delays end within a round trip and a synced write of
each other each vote for themselves and each answer the other against; each loses
its ballot as it reads that vote and canvasses again at once (ADR 0105), so the
election is longer by a second delay, the shorter of the two they draw, at most 500
ms. And a member that is heard and casts no vote is waited for, for the election
timeout (state 3).

**Messages during an election.** A member in init reads nothing. In every other state:
canvass position updates the sender's position in the reader's table, one whose log is
empty is answered in a follower's states and the candidate's as "A member that lost its
log" says, and a leader in
leader log replication or leader ready that reads one whose leadership term id, the
sender's candidate term, or log leadership term id is above its own term ends its
election in failure and begins again at init, counted in `cluster.elections_failed`,
while one in leader replay or leader init updates its table alone, the sender's next
canvass position reaching leader ready; a leader reads a canvass position whose log
leadership term id is below its own term, neither term above it, and answers with new
leadership term for the highest term in its record at or below that term, −1 when its
record holds none (ADR 0096), its next fields as the table gives them, the leader's term base log
position standing for the next term's base when that term is the leader's own, and
one at its own term updates its table alone during its election, since states 5 and 8
send new leadership term to every member. The `canvass-position` vectors pin each
case: the leader's term, its state and the message's two terms give its action,
answer, election or table. Request vote
for a candidate term at or below the reader's candidate term is answered with a vote
against, the candidate term unchanged, so that a member votes once in a term, a second
request for a term it has voted in answered against whatever the candidate's log; one
for a higher term from a candidate whose log is behind the reader's is answered
against; one for a higher term from a candidate whose log is at least the reader's is
answered for, in canvass, nominate, candidate ballot or follower ballot, and the reader
goes to follower ballot; in each of the last two cases the reader takes the request's
candidate term as its own and stores it before it sends the vote, so that a restart
between the two loses the vote and never the term; in init, once its election has
closed, when it holds an election instead ("Messages outside an election"), or in a
state other than those four with a higher term and a log at least the reader's, the
request is answered with nothing and the candidate term is unchanged. The `request-vote` vectors pin each
case: the reader's candidate term, log and state and the request's fields give the
vote, for, against or none, and the candidate term after it. Vote is read by a candidate in
candidate ballot and recorded against the voter with the candidate term it answers, and
the ballot counts the votes for the candidate's current candidate term alone, its own
among them, Aeron's `isQuorumLeader`, so that a candidate that takes a higher term
during its ballot, voting against a candidate whose log is behind its own, holds no
vote for its new term and goes back to canvass at once, never leading
a term with votes cast for another, and a candidate that reads a vote against its
current candidate term goes back to canvass at once too, since the ballot counts no
leader with one (ADR 0105); a message of any template read in a state other
than init marks its sender as heard for the reader's election, until the reader's next
init, and a candidate waits before the election timeout for the vote of a member it
has heard and of no other (state 3); the `ballot` vectors carry the candidate's term,
each vote's and the members heard, and say whether the candidate leads and whether
a vote against its candidate term has been cast, which loses the ballot at once; they
name no candidate, so a candidate that holds no vote of its own is not among them
(state 3). New
leadership term for the reader's candidate term, in follower ballot or candidate
ballot, or in canvass or nominate for a term at least its candidate term, is read against the
reader's log leadership term id, and one for any other term is ignored, counted in
`cluster.leadership_terms_ignored`, a term below the candidate term in canvass among
them, a departure from Aeron, which reads any term in canvass (ADR 0058), so that a member that has voted in a term follows no leader of
a lower one and never counts toward that leader's commits, Raft's rule: when the two
differ the reader goes back to canvass and takes neither the sender
as its leader nor the term, but for one case, a message whose log leadership term id
is below the reader's own and whose next leadership term id is above it: the leader's
record then holds none of the reader's terms above the one named, terms that fewer
than a quorum recorded, and the reader drops them, truncating its log first to the
base of the lowest of them when its append position is above it, by the rule of a
truncation below, under which a reader that knows a commit position above that base
truncates nothing, drops nothing and fails its election, and then taking every
term above the one named out of its record of terms, by term and not by base, since
the lowest is based where the log is cut, counted in `cluster.terms_dropped`; it
then reads the message again with the log it holds, whose term is then at or below
the one named: the one named, and it replicates or joins as below, or, when it
never recorded the term named, a lower one, and it canvasses as it does for any
message that is not for its log term, a ballot it was in being lost, and reports
that lower term, so
that a member's record becomes its leader's before it replicates or joins, Raft's
consistency check, and a departure from Aeron, whose leader names the term the
member reported whether or not its own record holds it (ADR 0096); a next leadership term id at or below the reader's log
leadership term id says nothing of the reader's terms from that one up, which may be
the leader's own, as in an answer to a report the reader made before it recorded
them or a copy from a leader it has since joined, and drops nothing; when they are the same the sender becomes the reader's
leader and the term its term, its candidate term too when the term is higher, in
memory alone ("The log"), and a
reader whose append position is above a next
term base log position the message carries, one not −1, truncates its log to that
position first, the uncommitted tail of an older term, `truncate recording` on each
recording that begins at or below the position and ends above it, which are two that
overlap after a catch-up (state 13; ADR 0095), and `purge
recording` on every recording that begins above it, every recorded term whose base is
above the position leaving its record of terms with them, so that it votes as a
member of no term it no longer holds; it sends every `purge recording`, from the
last recording back, before any `truncate recording`, and between the recordings and
the terms, after the last command is answered and before the first term is deleted,
it writes its log recordings file with the recordings that are left, so that a
member that stops inside a truncation starts again with one it can finish, and never
with a recording that begins past the end of the one before it ("A member's start";
ADR 0108); and a message whose
next term base log position is −1 truncates nothing. A member truncates only once
`cluster.log_recording_id` holds −1, which the duty cycle that left the log stores:
told to truncate before then, it cuts nothing, and the leader's next new
leadership term tells it again ("The service"; ADR 0103). A truncation is counted in
`cluster.log_truncations`. A reader that knows a commit position above the position
it is told to truncate to truncates nothing and fails its election, counted in
`cluster.truncations_refused` and `cluster.elections_failed`: a committed entry is
never truncated, and a leader that holds what was committed names no such position
(ADR 0083). A truncation whose command or write faults leaves the member's recordings,
its log recordings file and its record of terms cut as far as its steps went, and the
end of the log it reports as it was: the member **owes** that truncation, keeps its
position in memory alone, counted once in `cluster.truncations_faulted` with the
position in `cluster.truncation_owed`, and fails its election, counted in
`cluster.elections_failed`, at this fault and at any later one that finds it out of
init. Until a truncation to that position finishes, the member asks for it again at
the start of each duty cycle and before it reads a new leadership term, which it then
reads against the log the finished truncation left; it does not go to nominate (state
1); and it reads no new leadership term while the truncation is still owed, counted in
`cluster.leadership_terms_ignored`. So it neither leads, joins a log nor replicates a
term on an end it no longer holds, its log recordings file never names a recording
that begins past the cut, and a truncation a later message asks never cuts above the
owed position, where its recordings may end lower than the position asked (ADR 0148).
A duty cycle whose truncation faults again does nothing else, so a member whose
truncation goes on faulting stays in init, where it reads and sends nothing, until
`cavalry run` gives it up and it is started again. One whose truncation is under way,
its recording not yet given up, canvasses meanwhile, and answers request vote with the
log it reported before the fault, which holds at least what its recordings hold, so
that it votes for no candidate whose log is behind its own. A member that owes a
truncation and holds a vote floor does both: the floor's rule answers a request
vote, and a new leadership term is read by the floor's rule once the truncation has
finished ("A member that lost its log"). A member started again owes nothing: its
start makes its log directory agree with itself, as after a stop inside a truncation,
what the fault left uncut staying in its log, uncommitted, for the next leader's new
leadership term to ask the truncation of ("A member's start"). A reader
whose append position is then below the leader's term base log position does one of
three things: it goes to follower log replication with the stop position the message
gives, the next term base log position when its append position is below it, else the
next log position when the message carries one (state 9); it waits for a later new
leadership term when its append position is at the next term base log position and
no next log position is carried; or it fails its election, counted in
`cluster.elections_failed`, when the message carries no next leadership term id, −1,
since the leader's log does not reach its position. A reader whose append position is
at or above the term base log position goes to follower replay, recording the
leader's term at the term base log position first when its append position, after any
truncation, is at that base, before it reports, so that it counts toward the leader's
commits with the log it holds to the base, as a member joining in state 15 does. The
`new-leadership-term` vectors pin each case, the reader's candidate term and whether it
is in canvass among their inputs, the term recorded among their fields. A voter whose
log is empty, log leadership term id −1 at position 0, goes to follower replay or to
follower log replication on a new leadership term only in the two cases of "A member
that lost its log", "Taking a leader", and otherwise ignores it, counted in
`cluster.leaders_withheld`. Append position is recorded by a candidate or leader against the follower.
Commit position from the reader's leader raises the commit position it was told.

**Messages outside an election, and the leader's in states 5 to 8.** A leader reads
append position and records it; catch-up position and starts a replay, a `start
replay` of a recording of its own on stream 100 to `cavalry:udp?endpoint=<the
follower's catchup endpoint>`, the endpoint the leader's members string gives that
follower and not the one the message carries (ADR 0085), when the follower has none
running, a replay counting
as running from its `replay ready` until its `replay stopped`
(`spec/client-protocol.md`, "Events"); a catch-up position that arrives while a replay
the leader has stopped still counts as running is kept, one per follower, the newest
replacing an older, and started when the `replay stopped` arrives. The replay begins at the position asked in the
recording that begins at or below that position and ends above it, the one that
begins first where two do, and the leader tells the two requests apart by the
position: one below the leader's term base log position is a replication (state 9),
whose replay ends at the first term base log position in the leader's log above the
position asked, or at its recording's end where that comes first; a replay that has
stopped, whatever the reason its `replay stopped` carried, is followed by nothing of
the leader's own, and the follower asks again from its append position for what it
still lacks (state 9), so that the follower's image of each replay joins at the
position it asked from and finds a recording armed for it (ADR 0092); each replay
started is counted in `cluster.log_replications`, once per replay where a
replication spans several recordings, and the `replay-end` vectors pin where a
replay begins and ends; one at
or above it is a catch-up within the leader's term (state 11), whose replay follows
the live recording, counted in `cluster.catchups`; a position above the leader's own
append position asks for what the leader does not hold, and is answered by nothing
(ADR 0087); a position the leader's recordings
do not hold, purged by retention (`spec/client-protocol.md`, "Commands", purge
segments), is answered by nothing, and the follower's timeout counts it. Each replay's
publication has a session id of its own in this version, so the follower's image of
it is on its `catchup` endpoint alone; and sends stop catch-up, stopping the replay with `stop replay`, when a
follower reports an append position with the catch-up flag for the leader's term, whatever the position: the
follower sets the flag once it is on the live log (state 13), and holds every position
below where its live image joined. The replay is
asked to stop once, and stop catch-up is sent for each such report, so that one that is
lost is sent again on the follower's next (ADR 0087, ADR 0088). A leader, its election closed or in states 5 to 8, that
reads canvass position first stops any replay it has running to that follower with
`stop replay` and drops any next replay it was waiting to start for it, as a new
catch-up position from the follower does too, so that a follower that failed and
canvasses again never joins a stale replay or finds two started for one request;
during its election it then does with the message as "Messages during an election"
says; its election closed, it answers one whose leadership term id, the sender's
candidate term, and log leadership term id are both at or below its own term with new
leadership term for the highest term in its record at or below the reported log
leadership term id (ADR 0096), its next fields as the table
gives them, the three −1 when the reported term is its own, so that a restarted member finds
the leader and truncates no more than it must, and holds an election, beginning at
init, on one with either term above its own, Raft's step-down, counted in
`cluster.leader_step_downs`, a departure from Aeron, whose leader reads a canvass
position's log leadership term id alone (ADR 0058), so that a member whose candidate
term passed the leader's term, one that voted in a ballot the leader never saw,
rejoins through the election it brings instead of canvassing for as long as the
leader leads; the `canvass-position` vectors pin both. A follower in follower catch-up that reads stop catch-up for its term
and its own id, after it has reported with the catch-up flag, removes its catch-up subscription; one read
before that answers an earlier catch-up and is ignored (ADR 0088). A follower whose
election has closed, and that holds a log, answers a canvass position whose log is
empty with its own, to that member alone ("A member that lost its log", "Taking a
leader"; ADR 0145). Any member whose election has closed that reads
request vote whose candidate term id, or commit position whose leadership term id, is
above its own leadership term id holds an election, beginning at init, counted in
`cluster.leader_step_downs` when it was the leader and in `cluster.higher_terms_seen`
when it was a follower (ADR 0082); a leader in states 5 to 8 answers a
request vote for a term above its own as "Messages during an election" says, with
nothing when the candidate's log is at least its own and against, taking the term,
when the candidate's log is behind, the `request-vote` vectors in leader ready pinning
both, and holds no election for either until its election has closed; a follower
reads commit position from its leader as the leader's heartbeat and as its commit
position.

**The term's first entry.** The new leader appends the new leadership term event at
its append position, the term base log position, and the term's session messages and
events follow. A term's entries above the commit position at the time its leader was
lost may be truncated by the next leader, as new leadership term's rules say; a
committed entry is never truncated, since a quorum of members that have recorded the
leader's term holds it, each counted toward the commit only once it recorded the
term ("The commit position"), and no candidate whose log is behind such a voter's, by
the highest term in the record and then by position, gets that voter's vote. A session message in a truncated entry is lost, and so is
one the leader ignored ("Sessions"); the client learns of the first by the new leader
event and of neither by a message of its own, so a client treats a message with no
reply from the service as possibly lost and sends it again when its service's rule
allows, as Aeron's clients do.

**Standbys.** A standby is a member that the members string marks `standby`
("Members and streams"; ADR 0134). It is a follower. It canvasses, takes a leader's
new leadership term, replicates what it lacks, catches up, records the log, reports
its append position, reads its committed log for its sessions, takes the snapshots
its leader's cluster actions ask for ("Snapshots"), and answers a client's connect
with a redirect ("Sessions"), each as a follower that is a voter does. Two things
it does not do. It never goes from canvass to nominate, so it asks for no vote and
leads no term. And it answers a request vote with nothing, in every state, its
candidate term unchanged, counted in `cluster.standby_vote_requests`: a vote against
would end the candidate's ballot (state 3), and a standby has no vote to give. With
the request's term it does what a voter in its state does: once its election has
closed, a request for a term above its own is a ballot the cluster holds without
its leader, and it holds an election ("Messages outside an election").

A member counts voters, the voters of its own members string, wherever this section
counts members: for a quorum; for the quorum position and a leader's active members
("The commit position"); for a canvassing member's unanimous and quorum candidate
(state 1); and for a ballot, in which a candidate sends request vote to the voters,
counts their votes, and waits for those of them it has heard (state 3). A standby's
report, its append position and a vote it sent count toward none of them, and a
standby that is heard is not waited for. A member that is the only voter of its
string leads from init, as a cluster of one member does (state 0), and its standbys
canvass and follow it. The arithmetic vectors are given the voters alone.

A member reads a message of any member its string names as the message's template
has it read, whether its string marks the sender a standby or not, and sends what
it sends to every other member, standbys among them: canvass position, new
leadership term, commit position, the log and a replay. So a member votes, on the
logs compared, for a candidate that its own string marks a standby, and follows a
leader that its own string marks one. The mark changes what a member counts and
what a standby itself does, and nothing else. That is what lets one member's mark
differ between the strings of two members while the members are started again with
a new string, one after another.

A member waits in an election for no member that its own string marks a standby,
and no commit position waits for one. The pace of the log does. A
standby's `log` endpoint is a destination of the leader's log publication as a
voter's is ("Members and streams"), and the publication's `min` flow control counts
every destination. So a standby that takes the log slowly holds the leader's log
back as a follower that takes it slowly does, with the leader's claim back-pressured
and its ingress unread ("Sessions"), and a standby that sends no status message
holds it for up to the flow control receiver timeout and is then dropped from the
publication's table (`spec/udp.md`, "The sender").

**Changing a standby's mark.** A standby is promoted, and a voter is made a standby,
by a change of the members string, which each member takes when it is started with
it. Nothing else of the string changes while a cluster keeps its log: its members,
their ids and their endpoints stay ("Out of this version", dynamic membership).
Three rules keep every committed entry in the log of every leader across such a
change. No message carries a member's string, so no member checks them: they are
the operator's, and `docs/how-to/promote-a-standby.md` is the procedure that keeps
them.

1. One mark at a time. Two strings whose voters differ by one member have no two
   quorums apart: a quorum of the one and a quorum of the other share a voter, so a
   leader that either elects holds what the other committed ("Logs compared"). Two
   strings whose voters differ by two members do not have that. Of the voters 0, 1
   and 2 and the voters 0, 1 and 3, members 1 and 2 are a quorum of the first, and
   members 0 and 3 a quorum of the second.
2. A change is complete before the next begins. It is complete when every member
   that runs was started with the new string, all of them have closed their
   elections behind one leader among them (state 17), and a quorum of the new
   string's voters is among them. A member counts by the string it was started
   with, so a quorum of the new string's voters recorded that leader's term at its
   log position before its election closed (state 8), and holds every entry
   committed before that term. And no leader of a later term has committed by the
   string before. Such a leader had the votes of a quorum of that string's voters,
   which shares a voter with the quorum that follows this leader (rule 1), and a
   member that voted in a later term stored that term and follows no leader of
   this one ("Messages during an election"). The quorum is what lets the rule
   leave out the members that do not run. With fewer, the members that are
   stopped can be a quorum of the string before: they may have elected a leader
   of a later term among themselves and committed, while the members that run
   still follow a leader whose process stood still. One member that says it leads
   is not enough for the same reason: a leader parted from the others says so
   until its own clock has counted the leader heartbeat timeout, and a process
   that was stopped counts late. Until a change is complete the string before it
   is still one of two side by side, and a third would differ from it by two
   marks. A member runs until its process has ended. One that is parted from the
   others, or whose process stands still, runs, whether any member hears it or
   not, and while it runs with the string before, the change is not complete. An
   operator who cannot reach a member's host does not know that its process has
   ended, and makes sure of it before a change counts that member out.
3. No member is started with an older string than the newest that a member of the
   cluster was started with. The rule reaches a member when it is started. A
   member that runs on through a change, unheard, is never started and is rule
   2's.

`cluster.voters` holds which members a member's own string gives a vote, and
`cluster.standby` whether it marks the member itself, so that which string each
member runs can be read from its counters ("Counters"). Whether every member that
runs was started with the new string is read from `cluster.voters` of every one
of them, which is one value where the strings agree. Nothing on one host shows a
member that was started with an older string its host still holds, since that
member agrees with its host; one such member is enough for the next change to put
two members' strings two marks apart.

**A member that lost its log.** A member whose log directory was lost, or never kept,
as a container's that is on no volume, starts again on a new one: its record of
terms holds no term, its candidate term is −1 and its log holds nothing. No start can
tell it from a member of a new cluster. It has forgotten the votes it cast and the
entries it acknowledged, and an entry a quorum committed may have counted its
acknowledgement: its vote, given to a candidate whose log lacks that entry while the
members that hold it are stopped, would elect a leader without it (ADR 0104, ADR
0145). As a follower, it would take a leader that a later term deposed, and count
toward that leader's commits ("Taking a leader"). In Aeron, a member that lost its state is rebuilt from a backup by an
operator, and nothing in its election holds it back ([glossary](../docs/glossary.md)).
So such a member **withholds**: it never goes from canvass to nominate, and it answers
a request vote with nothing, its candidate term unchanged, counted in
`cluster.votes_withheld`, as a standby does ("Standbys"). It answers as a voter in its
state does in one case alone: when each voter of its members string has reported its
log in this election, itself and the candidate among them, the candidate's log in the
request, and the candidate's log is at least each ("Logs compared"), and when each
voter other than itself and the candidate has named in this election a term below the
request's candidate term id ("Taking a leader" says how a voter names one). A log at
least that of a member holding a committed entry holds the entry, as Raft's vote rule
has it, so such a candidate holds every committed entry that any voter still holds;
while a voter has not reported, it may hold one the candidate lacks. And a vote the
member cast before it lost its log, in the request's term, went to a candidate that
stored the term before it asked and names it in every message it writes after: with
each other voter below the term, that candidate can only be this one, so the member
votes no second time in a term, which two leaders of one term would need. A member
withholds in two cases, which the `withholds` vectors pin, and votes in the one above,
which the `withheld-vote` and `votes-in-term` vectors pin, the voters alone given:

- Its log is empty, log leadership term id −1 at position 0, while a member's report
  in this election, or the request it answers, holds a log, log leadership term id 0
  or more or a position above 0. A member whose log is empty beside members whose
  logs are empty too is a member of a new cluster, and votes as any voter does.
- It holds a vote floor.

The **vote floor** is the mark a member that took a leader with its log empty holds
until it has caught up. A member whose log is empty that takes a leader on a new
leadership term, going to follower replay or to follower log replication, stores that
message's log position, the leader's append position, as its vote floor before it
records a term, written and synced in its record of terms (`spec/durable-log.md`,
"Terms"); a message whose log position is 0 gives a floor of 0, which marks the member
all the same (ADR 0161). The member withholds for as long as it holds a floor. It
clears the floor once three things hold, since it stored the floor and in this run of
its module: it has joined a leader's live log, going to follower ready (state 16); its
append position is at or past the floor; and, from version 3, its append position is
at or past the log position of a floor answer it took (below; ADR 0163). The
`clears-vote-floor` vectors pin it. It clears it by writing the floor's slot as zeros
and syncing the file, counted in `cluster.vote_floors_cleared`, and it votes as any
voter does from then on.

**The floor answer.** From version 3, a member that holds a vote floor and has taken
no floor answer for the term it follows, in this run, sets the floor flag of every
append position it sends, in every state it sends one in. A leader of version 3 that reads an append position
with the flag, from a member of its members string, whatever term the report names,
answers it with a floor answer to that member alone, in every state in which it reads
append positions, at most once each leader heartbeat interval for each member,
counted in
`cluster.floor_answers_sent`: its term, its append position, read as it writes the
answer and not as an earlier read left it, the two member ids, and
the session id of the data frame the append position came in, the session of the
member's consensus publication to the leader. A member takes a floor answer that names
it, from the leader it follows, for that leader's term, with the session id of its own
consensus publication to that leader, and ignores any other, counted in
`cluster.floor_answers_ignored`; a member that holds no floor ignores one uncounted,
as one still on its way when its floor cleared is. It keeps the lowest log position of
the answers it took since it stored its floor, in this run, in `cluster.floor_answer`,
−1 for none, and stops asking until it follows another term. Any answer it took will
do: each was written after the leader read a report of the member's present run,
which the next paragraph says is all the clear needs, and a member that waited for
the latest would chase a leader that goes on appending. It asks again under each term
it holds no answer from, so that an answer of a leader that a later term deposed,
which the later leader's log may never reach, holds it only until that leader
answers.

Each condition holds entries of its own. The leader the member joins is one it took
by the rules below, or one a later election gave it, so it holds every entry the
cluster committed, by the vote rule ("Logs compared"); a leader that a later term
deposed, whose log would give the member nothing, is not taken. The live log reaches
the member in an image made at that leader's sender position ("Members and streams";
`spec/udp.md`, "The receiver", steps 1 and 2), and a follower goes to follower ready
only with its append position at or past its live image's join position (states 13
and 15).
So the join holds every entry the leader's sender had sent the member before it came
back, since a sender's position never goes back, and a later leader's term begins past
every such entry the cluster committed. The floor holds every entry up to the append
position the leader's duty cycle had read before it wrote the new leadership term the
member took, what its recordings had synced by that read, and no more: a leader's
module reads consensus messages before its duty cycle, so a message it writes carries
the position as the cycle before read it. Even for a message written after the
member's start, an entry the recording synced after that read, which a replay may have
brought the member's earlier run, stands above the floor, and so may an entry the
sender sent before the recording synced it, which the join holds. ADR 0161 said that
the floor of such a message held every entry the member had from a replay; ADR 0163
corrects it. The recorder is a spy, which reads the publication's log on the leader's
own driver (`spec/udp.md`, "The channel URI"), so it may hold an entry the sender has
not yet sent, and the live image may then join below it. For a message the leader's
sender held back from the member's earlier image ("Taking a leader"), or one at
position 0, the floor holds less still, and the join holds what the sender had sent. Follower catch-up's commit target holds a third set. A member had a
replay in its earlier run only from a leader whose log was above position 0, and a
held-back message it reads now was written after the one it followed then, so it is
above 0 too. The member, its log empty, catches up again, and does not leave follower
catch-up before its append position reaches the commit position it was told when it
read the leader's stop catch-up (state 13). None of the three holds an entry the member
had from a replay before the sender sent it, above the floor, as on the floor of a
message held back or of one whose duty cycle read the position before the recording
synced the entry, when the leader counts the member's acknowledgement of it so late that the commit position
the member was told then does not reach it: a leader keeps each member's last report,
whichever run of the member sent it. The floor answer holds it. Every entry an earlier
run of the member held came to it in one of two ways: from the leader's recordings, by
a replay, or from the leader's sender, on the live log. The answer holds the first,
since a replay of a recording reads no further than its extent, which for a live
recording is its synced position (`spec/client-protocol.md`, "Commands", start replay),
and the leader's append position, read as it writes the answer, is the end of what its
recordings hold synced then ("The log"): at or past every position a replay had read
before. The recorder writes past the synced position before each sync, so an append
position an earlier read left, as of the leader's last duty cycle, can stand below an
entry a replay has read since. The join holds the second. So neither condition can go while the
argument rests on both. An answer's session id is the member's present run's: a member
that held any of the log and starts on a new log directory starts on a new driver ("What
the second case rests on"), whose session ids begin at a random value
(`spec/client-protocol.md`, "Commands"), and a member adds each of its consensus
publications once in a run as an exclusive publication, so a run on a driver that ran
on has session ids of its own. An answer to an earlier run, held back or late, names
that run's session and is ignored. Two drivers' random ids that meet would name two
runs alike, and would also merge the two runs' frames into one session at the leader's
receiver (`spec/udp.md`, "The receiver", step 1). So an answer the member takes was
written after the leader read a report of the member's present run, and its log
position is at or past every entry a replay of the leader's recordings had read then,
to any run of the member. An append position
counts toward a commit only for the term it names ("Roles and the election", the quorum
position), so an acknowledgement of the member's earlier run commits only at the
leader it was sent to, while that leader leads that term. Any leader the member's
present run follows before its first vote holds every entry such an acknowledgement
commits, by the vote rule, since the member's own vote in any quorum that elected it
is the earlier run's, cast after its acknowledgement, or one it has not cast yet. A
member that cleared its floor therefore holds every entry a report of its earlier
run can commit, however late the report reaches the leader (ADR 0163). A
member reads its floor at its start with its candidate term, so a member that
stops before it has cleared its floor withholds after its next start until it has
joined the live log and taken an answer in that run, even with its append position at
or past the floor.
A torn store of the
floor reads as no floor, in a member that has recorded no term since, or as the floor
stored. A torn clear reads as no floor, which the clear was to leave, or, when it
wrote only bytes that were zero already, as the floor before it, which the member
holds until it meets the clear's conditions again in a later run (`spec/durable-log.md`,
"Terms"). Both are safe. `cluster.vote_floor` holds it, −1 for none.

**Taking a leader.** A leader that a later term deposed, and that has not heard of
that term, still leads with its election closed, and answers a canvass position whose
terms are not above its own ("Messages outside an election"). Its log lacks what the
later term committed. A member that kept its candidate term ignores that leader's new
leadership term, since it voted in the later term or followed it; a member that lost
its log has a candidate term of −1, which is above no term. So a voter whose log is
empty takes a leader on a new leadership term, and so stores its vote floor, in two
cases alone, which the `takes-leader` vectors pin, the voters alone given:

- It voted for that leader, in that term, in this election.
- Each other voter of its members string has been heard in this election, by a
  canvass position or by a new leadership term, the leader by the message itself, and
  none named a term above the message's leadership term id. A canvass position names
  its leadership term id, the sender's candidate term, and a new leadership term its
  leadership term id. The member keeps the highest term each voter named until its
  next init.

It ignores any other new leadership term, counted in `cluster.leaders_withheld`, and
its election goes on in the state it is in. The second case holds the floor. An entry
the cluster committed before the member lost its log was recorded by a voter other
than the member, which recorded the entry's term first and keeps it, so that voter
names a term at least the entry's. A leader of that term appended the entry, and a
leader of a later term holds it by the vote rule ("Logs compared"), so the leader's
log holds the entry, and the floor is at or past it. The case holds the vote as well:
a vote the member cast before it lost its log was for a candidate that stored the
term first, so that candidate names a term at least the vote's, and the member, whose
candidate term becomes the leader's, votes no second time in that term. The first
case is a vote cast by the rules above: no member the member heard held a log, or each
voter had reported a log the candidate's is at least and no voter but the candidate
had named the request's term or a later one. A standby takes a leader as
every member did before this rule, since its appends count toward no commit and it
has no vote.

What the second case rests on. Its argument reads a voter's term, and the leader's
log position, from the messages the member reads once it has lost its log, and a
message names what its sender held when it was written. So the case rests on two
facts about what the member reads from its start. First, it reads nothing that came,
in a peer's stream, before what its run before had read: a voter that recorded a
term before a message that run read names a term at least that one in every message
it wrote after it. Second, where its run before held any of the log, it reads nothing
that a peer's sender had sent before the start. So a new leadership term it reads
was written after the start, but for the frames the next paragraph names, and so
once the leader held every entry the member had acknowledged, and its log position
is past them. Both come from the consensus stream, each peer's
own publication to the member, read in the order written ("Members and streams"). A
member's driver records its log into its log directory, so a driver that ran on
through the member's stop holds a recording of the log's stream once the member held
any of the log, and a start beside it with no `terms` file is refused ("A member's
start"). A member that held any of the log and starts on a new log directory starts
on a new driver, then. Each peer's stream begins on it in a new image, made once the
member subscribes, by the peer's setup frame at the peer's sender position, and a
datagram below it is dropped (`spec/udp.md`, "The receiver", steps 1 and 2). A member
that held none of the log can start beside a driver that ran on. Its subscription
then joins each image of a peer's stream that the driver kept at the image's rebuild
position, which is past every frame a reader of the image has read (steps 3 and 5), or it
gets a new image, where the image was retired with the subscription of the run
before (step 5; ADR 0068). Either way the first fact holds, and the second is not
needed, since the member acknowledged nothing. A member that starts on the log
directory it kept, with an empty log, needs neither: it recorded no term, so it
counted toward no commit, and it keeps its candidate term, so it votes once in a
term.

One kind of frame is outside the second fact. A frame that a peer wrote before the
start and that its sender had not yet sent is sent to the new image. The sender holds
a frame back only while the readers of the image before are a receiver window behind
(`spec/udp.md`, "The sender", and "The receiver", step 4). A new leadership term
among such frames, written before the leader appended an entry the member
acknowledged, would give the member a floor below that entry. For that, the image
before must have read nothing of
the leader's consensus stream while the leader wrote a receiver window of consensus
messages to the member. And all that time the member must have gone on acknowledging
the leader's entries, which it does for at most the leader heartbeat timeout once it
hears nothing from its leader. A member whose log is empty reads such a frame only when
it answered a report of an empty log, so the member had lost its log once before then
too. The floor's clear holds that case for an entry the leader's sender had sent the
member: a member clears its floor only once it has joined the live log as well, whose
image the sender makes past every entry it sent the member before, so the member votes
only once its log holds the entry, whatever position the frame gave the floor (ADR
0161; work item 411). An entry the member had from a replay before the sender sent it
is held by the floor answer ("A member that lost its log"; ADR 0163).

A member whose election has closed sends no canvass position, so a member that lost
its log would not hear it. So a member that holds a log, log leadership term id 0 or
more or a position above 0, that does not lead, and that is in a state other than
canvass and nominate, answers a canvass position whose log is empty, log leadership
term id −1 at position 0, with a canvass position of its own, to that member alone.
A leader answers with new leadership term, as it answers any canvass position. A
member whose log is empty answers none, so no two members answer each other's
answers. A member that canvasses sends one every election status interval, 100 ms,
so each member answers it at most that often.

What it costs. While a member withholds, the cluster tolerates one failed voter
fewer: a cluster of three in which one member withholds elects only while both others
run, and when one of them stops it elects no leader until that one runs again, even
when what it held was never committed. A new cluster in which one member holds
nothing yet when the first leader stops is such a cluster, though the member never
held a byte: no member can tell that case from one in which it held a committed
entry and lost it. A voter whose log is empty takes no leader while a voter other
than itself and the leader is stopped or cannot reach it, unless it voted that leader
in, even when the leader is the newest term's: a cluster of three in which one member lost its log and another is
stopped has a leader and no follower of it, and commits nothing, until the stopped
member runs again. No member can tell that leader from a deposed one while the
member that would name the later term is stopped. Two members of three that lost their logs
while the third is heard elect the third, since every voter is heard. Two that lost
their logs while the third is stopped elect between them as a new cluster does: a
quorum's record is gone, which no rule of this page gives back. So do two members of
three that hold no log, one that lost it and one that never held a byte of it, as a
new cluster's member that started late and has not caught up, while the third, which
holds the log, is stopped. No member that runs holds what the cluster committed, and
neither can tell the cluster from a new one, whose members elect with one of three
not yet started; a rule that held this case would keep every new cluster from
electing until each of its members runs. And a member whose
leader's log no longer begins at position 0, its recordings purged below a snapshot,
cannot replicate what it lacks, and withholds until it is started from a peer's
snapshot, which this version does not do (work item 365).

**A member's start.** A member starts on a new log directory or on the one it kept
("The log"; ADR 0089, ADR 0108). On a new one its record holds no term, its candidate
term is −1 and its log holds nothing. On the one it kept it takes up its candidate
term, its record of terms and its log, which it reads back before it reads a
message, from the log recordings file of `spec/durable-log.md`, "Log recordings",
and the driver's catalog beside it. Its log is the recordings that file names, in the
file's order, from the file's floor position. A recording ends at the stop position
the catalog holds for it, and one that is still being made, the driver having run on
while the member was away, is stopped first, so that its stop is read once. Each
recording begins at or below the furthest end of the ones before it, the floor
position for the first, and the log ends at the furthest end of them all: the end
of the contiguous span, as "The log" defines the append position, which is not the
last recording's end when that recording ends inside one before it. The catalog may
lack the last recordings the file names, and no others but the first ones, purged
below a snapshot (below): a truncation purges a
member's recordings from the last one back, before it cuts any, and writes the file
after them ("Messages during an election"), so those are a truncation the member's
stop cut short, and the log ends at the furthest end of the recordings that are
left, when that end is at or above the position the member's read of its log
begins at (below). The member then writes the file with the recordings it took, when they are not
the ones it read, one being gone or holding nothing, and deletes from its record of
terms each term whose base is above the end of its log, as that truncation would
have; a start that found the file's last recordings gone or deleted a term is
counted, once, in `cluster.truncations_finished`, and recordings purged at the
head of the file, below a snapshot, are not. What
that truncation had not yet cut stays in the member's log, uncommitted, and the
leader's next new leadership term has the member cut it. Nothing else shortens the
log a member starts with.

A start that cannot finish leaves what a later start needs. A `stop recording` that
is refused, or whose stop the catalog does not show within the stop timeout, 1 s,
ends the start with its error; the recordings stopped before it stay stopped, and
the file still names them. A file the start cannot write or sync ends the start
before any term is deleted. A recording of the log's stream that the file does not
name is left as it is, stopped or still being made: it is no part of the member's
log, the driver ends it with its stream, and the member's next join of a log makes a
new recording.

A member that starts takes its consensus module's own state from a snapshot
("Snapshots"; ADR 0130). It reads its snapshots file (`spec/durable-log.md`,
"Snapshots") before it reads its log back, and takes the newest snapshot of the
consensus module, service id −1, that it can read whole. It can read a snapshot whole
when three things hold. The catalog holds the snapshot's recording, of the snapshot
stream, stopped and with bytes. The driver plays the recording back, on
`cavalry:ipc` and the log replay stream, within the snapshot load timeout
("Values"). And the recording holds the snapshot that the file's entry names: a
snapshot marker whose mark is begin, with type id 1, the entry's log position and
the entry's leadership term id; a cluster session for each session, at most 10, in
the rising order of their cluster session ids, each id 0 or more and below the next
session id of the consensus module that follows them; that consensus module; and a
snapshot marker whose mark is end, with the same type id, position and term. A
member's table of sessions takes those sessions whole, or the recording does not
hold a snapshot that a member writes: one with a response channel longer than a
session holds is such a recording. A snapshot that fails one of the three is
passed over for the one the file lists before it, and counted in
`cluster.snapshots_passed_over`. A member never begins its read at a snapshot's
position with another state than that snapshot's. The file lists at most 64
snapshots (`spec/durable-log.md`, "Snapshots"), and a start waits the snapshot load
timeout for each one whose recording the catalog holds and the driver does not
play back: 320 s at the most, on a driver that plays none of 64 back, before the
start reads its log from position 0.

From the snapshot the member takes its sessions and the next session id. Each
session is open as its committed session open event opened it on a follower
("Sessions"): with no publication, nothing owed, and its timeout counted from when
a leader begins to serve. The member's read of its committed log, which gives it
the rest of that state, begins at the snapshot's log position, which
`cluster.snapshot_start_position` holds while the member runs. With no snapshot
that it can read, the member holds no session, the read begins at position 0, and
the counter holds −1.

So a member starts on a log that it holds whole from the position its read begins
at, and it need hold nothing below that position. Three logs follow that a member
starting from position 0 does not start on. A file whose floor position is at or
below that position: the member forgot the recordings below the floor while it
ran. A file whose first recordings are gone from the catalog, when the first
recording that the catalog holds begins at or below that position: the recordings
below it were purged (`spec/client-protocol.md`, "Commands", purge recording), the
start leaves them out, and the file it then writes has that recording's start as
its floor position. And a file that names more recordings that hold bytes than a
member keeps, 1,024 (state 0), when the newest 1,024 begin at or below that
position: the start forgets the others, and the file that it then writes, which no
longer names them, is counted in `cluster.log_recordings_listed`;
`cluster.log_recordings_forgotten` counts the recordings a member forgets while it
runs. A member whose log begins above the
position its read begins at does not start, whichever of the three it is.

Nor does a member whose log ends below that position. The snapshot is of a position
that the member read from its committed log, which it reads from its own
recordings; the member listed it only once its log had synced to that position
("Snapshots"; ADR 0143); and a truncation never cuts below a commit position
("Messages during an election"; ADR 0083). So the log that the member left reaches
that position, whatever stopped the member, and a log that ends below it has lost
its end. Where the catalog lacks the file's
last recordings, they are then no truncation that the member's stop cut short:
recordings of its log at or above the snapshot's position were removed, as by a
purge of one recording too many. Where the catalog holds every recording the file
names, the end of the log was cut (`spec/client-protocol.md`, "Commands", truncate
recording), or the file and the catalog are older than the snapshot. A log that
ends at that position reaches it, and the member starts. Where the log ends is
known once the recordings still being made are stopped, so the start stops those
first; it writes no file and deletes no term. A member
that forgets a recording while it runs goes on, its state in memory, counts it in
`cluster.log_recordings_forgotten`, and starts again on that log directory only
from a snapshot of a position at or above where its log then begins. A log
directory therefore lasts a member for 1,024 recordings of its log after its newest
snapshot: one for each log it joins, each replay it replicates or catches up by,
and each term it leads.

A service reads its member's log from the log position of the snapshot it started
from, or from position 0 ("A service's start"). Beside a member whose log no
longer reaches back to that position its replay is refused, as a replay of a
position the member forgot is. So a member's log is needed from the lower of two
positions: the log position of the newest snapshot of its consensus module, and
that of the newest snapshot of a service that it lists. `cluster.log_purge_position`
holds that position, and 0 while the member lists no snapshot of one of the two:
the member's own start and a service that starts from its newest snapshot read
nothing below it. The position assumes a service whose program asks it to start
from its snapshot ("A service's start"). The member cannot tell whether it does,
and lists the snapshots of a service that never asks all the same; such a service
reads the log from position 0 at every start. Beside a service whose program asks,
an operator purges the recordings of a member's log that stopped at or below the
position, and none above it; beside one whose program takes snapshots and never
asks, none. Beside a member whose service takes no snapshot the position stays 0,
and nothing is purged. The position is of the newest snapshots the member lists. A
start that passed a snapshot over, the member's or the service's, read its log from
an older snapshot or from position 0, and the next start does so again: an
operator purges by the position only while `cluster.snapshots_passed_over` and
`cluster.service_snapshots_passed_over` of the member are 0.

A member does not start, and says why and what to do, in each of these cases. Its
record of terms holds a term and its log directory holds no log recordings file:
the directory is one a version before ADR 0108 wrote, or the file was removed, and
the member cannot tell its log from the other recordings in the catalog. Its log
directory holds a log, one that ends above 0, and its record of terms holds no
term: the `terms` file was removed or replaced, and the terms of the log and the
votes the member cast went with it. Its log directory holds no `terms` file at all and
the catalog holds a recording of the log's stream: a member records a term before it
records an entry, so the record was removed, whatever else the directory holds (ADR
0104). The file
fails a check of `spec/durable-log.md`, "What a reader rejects", or names another
member. Its log begins above the position its read of it begins at, as above, or
ends below that position though the catalog holds every recording the file names. Or
the catalog holds, in place of a recording the file names: no recording, before one
that it does hold, which is no purge below its snapshot as above; no recording, nor
any that the file names after it, where the recordings before it end below the
position its read of the log begins at, as above; a recording the
driver found damaged; a recording that
begins elsewhere or is of another stream; or a recording that begins past the
furthest end of the ones before it. In every one of them a start would give the
member a log shorter than the one it reported, or a module that does not know its
sessions, and with a member whose log is behind it is a quorum that can elect a
leader without an entry the cluster committed. A member started on a new log
directory beside the others is safe while each other member holds the log it kept,
a new leadership term its leader's sender held back by flow control among the
frames it reads ("Taking a leader"; ADR 0161, ADR 0163). It takes a leader once it has heard
every other voter, or voted that leader in, it joins and replicates the log, and it
withholds its vote until it has caught up past its vote floor and past its leader's
answer to its present run, and joined the live log, which clears the floor ("A member
that lost its log"; ADR 0145, ADR 0161, ADR 0163). It waits, taking no leader,
while another voter is stopped. And beside a member that holds no log either, one
that never caught up, while the member that holds the log is stopped, the two elect
as a new cluster does, as "A member that lost its log" says. So a cluster that has
lost a member's log directory starts that member again on a new one and is back to
its full count once the member has caught up. Where its leader's log no longer begins at position 0, the member cannot catch
up, and the cluster is brought back to its full count by starting it again as a new
cluster, every member on a new log directory. A member that does not start counts nothing: it adds its counters once
it has read its log, and its error says which case it met.

The members of a cluster are all of a version that elects. A member of a version
before ADR 0104, whose leader is the member with the lowest id, casts no vote and
follows no leader a ballot made, and no message tells the two apart. A log directory
that version wrote holds a record of terms and no log recordings file, so its member
is refused here. A cluster of that version comes to this one as a new cluster, every
member on this version and on a new log directory.

What a start holds after a power loss. After a power loss or a crash of the
operating system, the driver's recovery cuts each recording that was being made to
the synced position its time index holds (`spec/durable-log.md`, "Time index" and
"The driver"; ADR 0144): the frames past it, which the loss may have left with a
header and no body, are dropped, and the recording ends at what its recorder had
synced. The driver does the same after a crash of its process alone, since no start
can tell the two apart. A member's append position is what its recordings have
synced ("The log"), so after a crash of any kind a member's log holds every entry it
reported and no frame it did not sync: as leader it replicates no frame whose body
was lost, and as a follower it keeps none in its copy of the entries the others
hold. A member whose host lost power, or whose operating system crashed, starts on
its log directory again as after any stop, and `recording.cut_to_synced` on its
driver counts each recording that recovery cut. Its log reaches its newest snapshot,
since a member lists a snapshot only once its log has synced to the snapshot's
position ("Snapshots"; ADR 0143). The commit position is a quorum's append
positions, so when every member's host lost power together the members started again
still hold every committed entry among them. A recording that a driver of version 4
of the durable log or before was making at the loss has no synced position, and
recovery keeps what its files hold, as that version did: a member whose host lost
power while it ran such a driver is not started on that log directory again, under
any version, and starts on a new one, as a member whose log directory was lost does
("A member that lost its log").

**Versions and the upgrade.** A cluster's members may be of different versions of
this page while their major is one, as they are during an upgrade (`CLAUDE.md`,
invariant 3; ADR 0136). Within a major no version changes the layout of a message
or of an entry, a template's id or the values of an enumeration. And none changes,
for a member, a client or a service of an earlier version of the major, which
messages it rejects ("What a reader rejects") or what an entry does to the state
it computes from the log. So a member reads every message it is sent by the
layouts of its own version, the header's version is 0 in every version of major 0,
and every member's log holds the same bytes at the same positions, whatever the
versions of the member that wrote it and of the members that recorded it. A
version of a major may add a message, a record of a snapshot, a use for an
entry that every version of the major names and accepts, or a use for a reserved
bit of a `flags` field, which every version of the major ignores, and the checks
its own readers make of them, where no member, client or service of an earlier
version is sent what it rejects. The version that adds says when a leader may append the
entry, and what an application does before its program first makes use of it (ADR
0136; ADR 0151). A version that must change a layout, a template's id or an
enumeration's values, or which messages an earlier version rejects or what an
entry does to the state an earlier version computes, is another major. It writes
another header version, which a reader of major 0
rejects in every message ("What a reader rejects"), and its members form no
cluster with members of major 0.

A member writes its own version in canvass position and in request vote, a client
in session connect, and a member in each session event it sends. A member reads the
`protocol version` of a canvass position and of a request vote for its major alone
and does nothing else with it: a version is no part of "Logs compared", and a member
votes for a candidate, follows a leader and counts toward a quorum of any version of
its major by the rules of this section alone. No other consensus message carries a
version. A member holds its own version in `cluster.protocol_version` ("Counters"),
and holds no peer's: `cavalry doctor` on each member names the version that member
runs (ADR 0137). A leader refuses a session connect, and a client reads a session
event, by the major alone ("Sessions"), so a client of either version opens a
session with a leader of either. Version 1 adds nothing that a member of version 0
must not be sent: a leader of either version appends the same entries, and a
follower of either records them.

Version 2 adds by that rule ("The service", "Timers"). Schedule timer and cancel
timer pass from a service to the member on its driver, which are of one build
(below), and to no other member. A leader of version 2 appends a timer event
whenever a timer it holds falls due, whatever the versions of the other members: a
member of every version of major 0 accepts the entry and records it, and a leader
of version 0 or 1 appends none, so timers wait while one leads. A service of
version 0 or 1 has no timer, since its program has no call that schedules one, and
hands its program every timer event as an entry of a kind the program does not
know (`spec/c-abi.md`, "Functions", the cluster service), as it did. So a program
that schedules a timer computes another state from a log than a program beside a
service of version 1 does, which is handed no timer: it is another version of the
application. An application's program therefore schedules its first timer only
once every member, with its service, runs version 2, and from that entry on no
member is stepped back to version 1. Nothing in a member checks either: a cluster
that breaks the rule has services whose programs differ, as one has whose members
run two versions of a program. The timer record is in a service's snapshot alone,
which the service beside the same member reads: a service of version 1 that is
given a snapshot holding a timer, as beside a member stepped back against that
rule, rejects the record with the reason `template`, passes the snapshot over and
reads the log from position 0 ("Snapshots", "A service's start"). No file of
`spec/durable-log.md` differs between versions 1 and 2.

Version 3 adds by that rule ("A member that lost its log", "The floor answer"; ADR
0163). A member of version 2 ignores the floor flag of an append position, and a
leader of version 3 sends a floor answer only to a member whose append position set
the flag, so no member of an earlier version is sent one. A leader of version 2
answers no flag, so a member of version 3 that holds a vote floor clears it only
once it has taken an answer of a leader of version 3 in this run, and withholds until
then. A member of version 2
that holds a floor clears it by the two conditions of ADR 0161 beside a leader of
either version, as it did. A member of version 3 stepped back to version 2 can still
be sent an answer that its run of version 3 asked for, in flight at its stop. It
rejects the answer with the reason `template` and counts it in
`cluster.messages_rejected`, and nothing else follows: the step-back is against the
rule above, as a step-back from version 2 with a timer is. No file of
`spec/durable-log.md` differs between versions 2 and 3.

A cluster whose every member runs, with its election closed, is upgraded one
member at a time, with the members string it has, each member on the log directory
it had. A member is stopped with the service beside it,
and both are started at the new version on the directories they had ("A member's
start"): a member's driver, its consensus module and its service are of one build,
since what passes between them is `spec/client-protocol.md`'s and not this page's.
The next member is stopped only once the one before leads, or follows the leader
with its election closed. In a cluster of three voters or more a quorum then runs
at every step, so the cluster commits throughout but for the election that the
leader's stop holds, whose candidates and voters may be of both versions; a cluster
of fewer voters commits nothing while one of them is stopped. No step loses a
committed entry in either, since a stop in an upgrade is a member's stop as any
other and each member keeps its log. A member of version 1 starts on the log
directory that a member of version 0 wrote,
and a member of version 0 on one that version 1 wrote, since no file of
`spec/durable-log.md` differs between the two: a member may be stepped back. What
"A member's start" refuses of a log directory is refused in an upgrade as at any
start. A follower of a build before ADR 0145 sends no canvass position to a member
whose log is empty ("A member that lost its log", "Taking a leader"), so a member of
a later build started on a new log directory beside it takes no leader until that
follower canvasses in an election or is upgraded: an upgrade stops no member on a new
log directory, and a member that lost its log directory during one waits. A member of
a build before ADR 0161 never clears a vote floor, and votes and stands once its log
is at or past it. A member of a later build holds a floor that an earlier build
stored, and withholds, until it joins a leader's live log, which an upgrade one member
at a time gives it once its election closes. A cluster whose members are stopped and
started on the later build all at once, with a floor held by every member a quorum
could elect, elects no leader until one of them runs the earlier build again, and the
others a build of ADR 0161 of version 2, which clears its floor at the join with no
answer, before any of them runs version 3 ("A member that lost its log"; ADR 0161, ADR
0163). An upgrade to version 3 begins once no member
holds a vote floor. A member that holds one when an upgrade has begun, one that lost
its log directory during it, withholds as above. If it runs version 3 and the
leader version 2, its floor clears only under a leader of version 3: in a cluster of
three voters no leader is elected while that leader is stopped for its upgrade, the
cluster elects again once it runs version 3, and the floor clears under the first
leader of version 3, which `cluster.vote_floor` reading −1 shows ("A member that lost
its log"; ADR 0163). `conformance/upgrade/` runs this upgrade on the loopback, from the last tree
of the version before to the tree under test, and holds every acknowledged entry
to every member's log (`conformance/README.md`).

**The leader's own entries.** Before the sessions of "Sessions" exist, the leader's own
process appends for itself: once its election has closed it may append a session
message whose cluster session id is −1, no session's, with its term, the cluster's
time and a payload of its own, which a service reads as any session message and
answers to no session.

## Sessions

A client opens a session by sending session connect on the ingress stream to one
member's `ingress` endpoint, from a publication of its own, after subscribing to the
egress stream on its response channel. A follower answers with a session event of code
redirect, its leader member id and the ingress endpoints as its detail, and opens
nothing; a member with no leader answers the same with leader member id −1. A member
that leads a term whose election has not closed, or that has not yet read its own
log to its term base log position, answers nothing yet: it leaves the
connect unread on its ingress image until it serves sessions or its election fails,
since a redirect to itself would only come back (ADR 0115, ADR 0116). The leader
answers: code error with the detail `invalid client version` when the connect's version
has a major other than its own; code error with the detail `invalid response channel`
when the response stream id is not 102 or the response channel is not
`cavalry:udp?endpoint=<host:port>`, which one that names any other key or is longer
than 280 bytes, the prefix, a host name of 253 bytes, `:` and five digits, is not; code error with the detail `concurrent session
limit` when its sessions, open and connecting, are at the bound ("Values"); otherwise it
gives the session the next session id, appends a session open event, and once that
entry is committed sends the session event of code ok with the session id, the
correlation id and an empty detail. The session is open to its client from then. A service may
reply to the session from its session open event on ("Replies"), on a publication of
its own, so a session message can reach the client before the ok event does. A
client keeps a session message it reads before its session event, for the session
it opens: while it waits for the answer to its connect it takes no session message
and nothing behind one on the same image, and nothing that follows its answer, and
the session it opens reads them in its polls and hands over each session message
that carries its session id (ADR 0118). A session is
opened by its committed session open event and closed by its committed session close
event, on every member that reads them in its own log ("Roles and the election",
states 6 and 10), the leader among them: the leader that accepted a connect holds the
session as connecting until it reads the entry at or below its commit position, and a
follower holds the open sessions with no publication, so that a member that wins an
election has them. The member's consensus module reads its own committed log for those
entries as a service reads it, through a replay of its recordings on the log replay
stream and no further than its commit position ("The service"), in every role and
every state of its election, from the log position of the snapshot it started from,
or from position 0 when it started from none ("A member's start"; ADR 0116, ADR
0130). A member that
wins an election serves sessions, which is to accept a connect, append a session's
message and send the new leader event, from when its election has closed and that read
has reached its term base log position, which its commit position reaches once a
quorum has recorded its term ("The commit position"). It has then read every session
open and close event of the terms before its own, before it accepts a connect, and
while it serves it reads only the entries of its own term. A session whose session
open event is of a term before the leader's own is one that leader sends a new leader
event when it begins to serve, and no ok event. The next session id is one more than
the highest the member has given or has read in a session open event, so that a new
leader gives no id twice (ADR 0115, ADR 0116). A message of the ingress stream is one
frame in this version, as an entry of the log is: a session message carries at most
1,344 bytes of payload after its header. No member reassembles, and no leader
appends an entry of more than one frame: a session message whose payload is longer
than 1,344 bytes, whether it arrives whole in one frame, which a sender whose MTU
length is above 1,408 bytes delivers, or as a fragment, is counted in
`cluster.messages_rejected`. Every member takes it from its ingress image, so that
it holds back no other client's message, and acts on nothing of it: it is not
appended, and it is its session's activity no more than a message that was ignored.
A member reads its ingress stream so that a message it cannot take yet stays unread
(`spec/log-buffer.md`, "Reading", the controlled read), and until that message is
taken it reads no message of another client's image. Every refusal is
counted in `cluster.sessions_refused`. A redirect or a refusal is answered on an egress
publication to the connect's response channel on stream 102, the one egress stream of
this version, whatever response stream id the connect named; the member holds the
publication until the answer is sent or the session connect timeout has passed since
the connect arrived, then closes it, an answer unsent by then counted in
`cluster.connects_dropped`. A member holds at most the pending answers of "Values" at a
time, and a connect that arrives with no room is dropped unanswered and counted in
`cluster.connects_dropped`, the client's connect timeout reporting it; so is a connect
whose response channel the driver refuses, one that is not `cavalry:udp`, is malformed
or names a host that does not resolve, since no publication to it can exist; the
driver's refusal runs before the leader's own check of the channel's form, so such a
connect is dropped and never answered `invalid response channel`. An open session whose ok event the leader has not sent within the session timeout, its
client having heard nothing and so sent nothing, is closed as a timed-out session is:
its session close event is appended with the reason timeout, and the ok event is not
sent after it (ADR 0115). A client that
has no session event within the session connect timeout reports the connect lost to
its caller and may send it again with a new correlation id; one that reads a session
event whose version has a major other than its own reports the cluster's version to
its caller and sends nothing more.

An open session carries session messages from the client, each a session message header
with the client's term and session id and the payload; the leader appends each as an
entry with its own term and timestamp, in the order they arrived, and ignores one for
a term other than its own or a session that is not open, counting it in
`cluster.ingress_ignored`. When the log publication's claim is back-pressured, under
`min` flow control with a slow follower or at the publication window, the message
whose claim failed stays unconsumed, the ingress image's position before it, and the
leader reads nothing more from its ingress images until a claim succeeds and the
message is appended: it buffers no message and drops none, so that the back pressure
reaches the client's publication and its caller (`spec/log-buffer.md`, "Claim, commit
and abort"); an entry of the leader's own, a session open or close event, a timer
event or the new leadership term event, waits the same way and is tried again on a
later duty cycle. A session open event the log has not taken at the session connect
timeout is given up with its connect, counted in `cluster.connects_dropped`, and so is
a connect the leader had accepted and had not read back as committed when it stopped
leading; a session open event that was appended and that a later leader commits opens
a session no client knows, which the session timeout closes (ADR 0115). The client sends session keepalive when the keepalive interval has passed since it
last sent anything; a session with nothing from its client for the session timeout is
closed: the leader appends a session close event with the reason timeout and, only
once the log has taken that entry, never before, sends a session event of code closed
with the detail `TIMEOUT`, as Aeron's `SessionManager.checkSessions` does. That closed
event is owed as an ok event is: tried on each duty cycle until the session's egress
publication takes it, and sent once. The debt ends when the session close event is
committed, which frees the session, or when the leader stops leading; a client that is
never told finds its leader silent for the leader heartbeat timeout. The log's taking
of the session close event is its append on the leader, not its commit: a leader that
appends the event, sends the closed event and stops leading before a quorum holds the
entry leaves the session open in the log, and the member that leads next takes it up,
sends it a new leader event, and closes it again by the session timeout, since a
client that was told its session is closed sends nothing more (ADR 0116). A client closes
its session with session close; the leader appends a session close event with the
reason client action and sends nothing. From when the leader has decided a session's
close, by the client's session close or by the timeout, it sends the session nothing
it still owed, an ok event or a new leader event, but the closed event of a timeout;
it ignores what the client sends for that session, each counted in
`cluster.ingress_ignored`; and the session stays open on every member until the
session close event is committed (ADR 0115). A service's replies to a session are
session messages on the egress stream, on a publication of the service's own, from
the service beside the member that led the term of the entry it answers, while that
member leads the term still, and from no other. They are in order from one service,
and there is no order between the services of two terms' leaders ("Replies"). A
client hands its caller each reply with the leadership term id and the timestamp of
its session message header, and hands over no stale reply: one whose leadership
term id is below that of a reply it has handed its caller before, on that session.
It reads a stale reply, counts it in `cluster.replies_stale` ("Counters") and
passes it over, so that the terms of the replies a session's caller reads never
fall. Only a reply it hands over moves the term it compares with: a new leader
event does not, and neither does a session message of another session (ADR 0121). When a term begins, its leader sends a new leader event
to every open session with the ingress endpoints, whether or not the leader changed,
since the clients' messages carry the term and the leader ignores one for another
term; the client sends its next message, a keepalive first, with the new term to the
new leader's `ingress` endpoint. A leader sends an open session nothing of its own
but the events of this page, and no heartbeat, so a client cannot tell an idle leader
from a lost one by what it reads. It learns that its leader is lost when the image of
the leader's egress publication on its response channel is no longer available
(`spec/client-protocol.md`, "Events", unavailable image). It keeps its session, goes on
offering its keepalive to the member it knows, and waits for the new leader event of
the member that leads next, which holds the session from its log. A client that has
had none for twice the leader heartbeat timeout its session event gave it, 20 s, since
it lost that image reports the session closed to its caller and sends nothing more:
the cluster has no leader, or cannot reach the client, and the caller may connect
again. In Aeron, this is the client's new leader timeout (ADR 0117). The session ids and open sessions are the log's: a
new leader recovers them from the session open and close events it has replayed, so
that a session survives its leader, and counts each session's timeout from the close
of the election, not from the session's last message under the old leader. A
keepalive, a close or a session message for another term, or for a session that is
not open, is ignored and counts as no activity, each counted in
`cluster.ingress_ignored`.

## The service

The service is a client of its member's driver, through the client API and the C ABI,
as the consensus module is. It reads the member's own recordings of the log, one at a
time, each through a replay that follows the recording, `start replay` with a length
of 2^63 − 1 (`spec/client-protocol.md`, "Commands"), into a `cavalry:ipc` subscription
of its own on the service log stream ("Members and streams"), from the position of an
entry: 0, the log position of the snapshot it started from ("Snapshots", "A service's
start"), or the position after the last entry it applied. The member names its live
recording in `cluster.log_recording_id` ("Counters"): the one it records the live log into, the
leader's recording of its log publication (state 5) or a follower's recording of the
leader's image it joined last (state 15), and never the recording of a span it
replicated or caught up (states 9 and 13). The counter is −1 while the member records
no live log: before its first such recording, and from when it leaves the log (state
0) until it has joined it again. The
path is the same on the leader and on a follower, and a service may start before its
member, with it or after it (ADR 0066). A service reads its own replay's session on
that stream and reads past any other, which is another service's.

A member keeps five rules of order for the service. It adds `cluster.log_recording_id`
and stores −1 in it before it adds `cluster.commit_position`, and a service reads the
recording's id only of a member whose `cluster.commit_position` it has found: a
counter holds 0 between its allocation and its first store, and 0 is a recording id.
It stores another value in `cluster.log_recording_id` before it stores a commit
position that the recording does not hold. And it writes the file of its log's
recordings ("The log's recordings") before it stores a recording's id in
`cluster.log_recording_id`, each time it names a recording, and truncates no recording
of its log until it has stored −1 there (ADR 0103). And it adds
`cluster.leadership_term_id` and `cluster.append_position` before
`cluster.log_recording_id`, and in every duty cycle stores each before it stores a
recording's id, so that a service that has found a member's recording loads a term
and an append position no older than the naming of that recording ("Replies", below;
ADR 0118). And it adds `cluster.leader_member_id` before `cluster.log_recording_id`
too, and in every duty cycle stores its term, then the id of the member it leads as
or follows, and both before it stores a commit position. So a service that reads an
entry below a commit position it loaded, and then loads that id and then the term,
takes no member that leads a later term, or follows in one, for the leader of the
entry's term: the member stored the later term before the id that says so
("Replies"; ADR 0118).

The member's log below its live recording is in the recordings that file lists, and
a service takes each position from one recording by the rule of "The log": the first
recording in the file that holds the position, and the live recording when none does.
It reads the file when it starts a replay, after it has loaded
`cluster.log_recording_id`. A file whose live recording id is not the id it loaded is
of a member that has since named another recording or none, and the service starts no
replay: it finds its member's recording again. With no file the service loads
`cluster.log_recording_id` again, from a record that is the member's by the test of
the next paragraph. When it holds the id the service loaded before, the member is of
a version that writes none, and the service replays the live recording. When it holds
another value, or the member has no such record, the member closed or left its log
between the two loads, and the service starts no replay: it finds its member's
recording again. A file that fails a
check of "What a reader rejects" is reported to the service's caller, and nothing is
replayed.

The service reads entries up to the commit position and no further: it polls the
replay's image bounded by the member's `cluster.commit_position` ("Counters"), a read
that delivers no frame that begins at or past the bound (`spec/log-buffer.md`,
"Reading"), so that a service never reads an entry a quorum does not hold; and since a
replay of a live recording reads no further than the recording's synced position, it
never reads an entry its own member does not hold durably. The service takes a commit
position as its bound only when two checks hold after it has loaded it. The counter's
record is the member's: allocated, of the counter's type, and with the member id as
the first four bytes of its key (`spec/counters-file.md`, "Metadata record"). And
`cluster.log_recording_id`, loaded after the commit position, is the id the service
loaded before it read the file, the member's live recording then, from a record that
is the member's by the same test, whether the service replays that recording or one
below it. The one check serves both: a member cuts no recording of its log while it
names a live one, so what a replay has played from a recording the file lists is
still the member's log at those positions when the commit position is loaded.
From the first load at which either fails, because the member left the recording or
its consensus module closed and the driver freed its counters, the service reads the
counters no more and keeps the highest commit position it took. The bound never falls,
and neither a record the driver has given to another counter nor a commit position of
a later log is ever a bound. When the recording the service replays stops, or has
stopped, the replay ends at the stop position; the service has read no further than
its bound, and it starts a replay again, from its own position, when a member names a
recording: of the next recording that holds that position, or of the live one.

A member that left the log and joined it again names a new recording. That recording
starts at the join position of the image the member joined, the member's append
position (state 15), and holds nothing below it, so the member's log is then more
than one recording ("The log"), as it is after a later term and after a span the
member replicated or caught up. A service whose position is the live recording's
start reads on from there. A service whose position is below it reads the recording
the file lists for that position to its stop, and then the next: a service that
starts at 0, or one whose member had recorded entries past the last commit position
the service took when it left the log. Those entries are delivered once the member's
commit position covers them, and not before. The driver withdraws a replay's image
the publication linger after the replay ends, so a service whose bound stays below a
stopped recording's end for that long starts the replay of that recording again. A
position that no recording in the file holds and that is below the live recording's
start is one the member's table has forgotten, past 1,024 recordings: the replay is
asked of the live recording, and `start replay` refuses it with code 23
(`spec/client-protocol.md`, "Commands"), which the service reports to its caller.

A position the live recording has not reached is no log yet, and no error (ADR
0135). A follower's catch-up may end with the member's log ending in its recording
of the replay, above what its live recording has synced ("Roles and the election",
state 13): the replay is sent to that member alone, and the leader's log
publication, which is `fc=min` ("Members and streams"), goes no further than a
receiver window past its slowest receiver (`spec/udp.md`, "The sender"). The file
lists the replay's recording with the
position it had synced when the file was written, so a service that has read that
recording to where it stopped takes the position it ended at from the live
recording, by the rule of "The log's recordings", and the live recording does not
hold it yet. So before a service starts a replay of the live recording at a
position that is a multiple of 32 and, when a file says where that recording
begins, at or above the live start position, it loads that recording's
`recording.synced_position` (`spec/client-protocol.md`, "Counters") from a record
that is the recording's after the load: allocated, of the counter's type, and
with the recording id as the first eight bytes of its key. While the value is
below the position, the service starts no replay and sends the driver no command:
it finds its member's recording again, as it does while the member names none,
and it reads on once the live recording has synced to the position. When the
driver holds no such record the recording has stopped, though the member names it
still, which it does until it has left its log. The service then asks for the
replay, and when `start replay` refuses such a position with code 23 and a file
says where the live recording begins, that is the same answer, no log yet: the
recording stopped below the position, and the file the member writes when it
names its next recording lists the recordings that hold it. Every
other refusal is reported to the service's caller, whatever the position, and so
is code 23 for every other position: one that is not a multiple of 32; one below
the live start position, as the paragraph before this one says; one of a member
that writes no file, whose live recording has stopped; and one in a recording the
file lists, which that recording no longer holds once it was purged
(`spec/client-protocol.md`, "Commands", purge segments). A member's live recording
is never purged while it is live: the driver refuses that with code 24. One that
has stopped while its member names it still can be, and for a position that such
a purge took, at or above the live start position, the service's answer is no log
yet for as long as the member names that recording; once the member has left its
log its file lists the recording, and code 23 is the caller's. An operator purges
a recording of a member's log while the member is stopped, when it names no
recording (`counters.toml`, `cluster.log_purge_position`): a service whose
position such a purge took is told so by code 23 once its member names a
recording again, and one that is ahead of its member's live recording waits.

It reads session messages, timer events,
session open and close events, cluster actions and new leadership term events in log
order, each entry once, with the cluster's time as each entry's timestamp field and
from no clock of its own, so that every member's service reaches the same state at
the same position, the replay check of invariant 1 (`CLAUDE.md`). A read ends at a
cluster action whose action is snapshot, where the program may give the service
its state ("Snapshots", "A service's snapshot"). The service beside
the member that led an entry's term replies to a session, while that member leads
the term still, with a session message on the egress stream; the service beside
every other member reaches the same state and sends nothing ("Replies", below). A service with a version
writes it as the app version field a leader announces, and a member whose service's
major version differs from the one new leadership term carries fails its election
from the state that read it, counted in `cluster.app_version_mismatches`, and begins
again at init, so that it never joins a log its service cannot apply; `doctor` names
the two versions.

### Replies

A service replies to a session with a session message on the egress stream ("The
session protocol"), with the leadership term id and the timestamp of the entry it is
reading and the id of the session, which is any session open at that entry: one that
a session open event at or below it opened and no session close event below it
closed. So a service may reply to a session at the session's own open event, and at
its close event. It
sends the reply on an exclusive publication of its own to the session's response
channel, one for each open session, at most the sessions of "Values". A reply is one
frame, a payload of 1,344 bytes at most, and a longer one is refused to the service's
caller (ADR 0118).

One service answers an entry: the service beside the member that led the entry's
term, and only while that member leads the term still. The term's new leadership
term event, the term's first entry, names that member, and a service compares it
with the member id that its member's counters are keyed by. So which service may
answer an entry is read from the log, the same wherever and whenever the entry is
read. The service beside every other member reaches the same state and sends
nothing.

The service beside that member answers the entry when its member says, as the
service reads the entry, that it leads the term: the member's
`cluster.leader_member_id` holds the member's own id, and then its
`cluster.leadership_term_id` holds the entry's term. The service loads the two in
that order, after the commit position that let it read the entry, each from a
record that is the member's by the test above ("The service", the fifth rule of
order). A member that holds an election has no leader, one that follows has
another, and one that leads a later term has that term: the service beside each
sends nothing for the earlier term's entries, and no other service answers them.
So the replies to the entries that a leader's service had not read when its member
stopped leading the term are lost at every change of leader, also when the same
member leads the next term. A client that needs an answer waits for one no longer
than it chooses, and sends again.

A session's replies are in order on one service's publication, and there is no
order between two services' publications. A leader that has lost its quorum leads
its term, by its own counters, until it holds an election, which is within the
leader heartbeat timeout of its quorum's last report ("Values"), and its service
answers what it reads of the term's committed entries until then. When the next
term's leader is elected and its service answers before that, a reply of the
earlier term can reach a client after a reply of the later one. Each reply carries
its leadership term id ("The session protocol"), by which the two are told apart:
the client hands its caller each reply with its term, and passes over, counted, a
reply of a term below one it has handed over ("Sessions"; ADR 0121). So once a
session's caller has read a reply of a term, it reads no reply to an entry of an
earlier term. A leader appends a session's message only in the term the message
names, so a reply that a service makes at a message's own entry carries the term
the client sent the message with. Where a service answers each message so, a
message the caller sent in an earlier term, and that has no answer yet, has none
coming.

A service does not answer history. When it first opens its member's log it loads
the member's `cluster.leadership_term_id` and then its `cluster.append_position`,
each from a record that is the member's by the test above. An entry of a term below
that term, or of that term and below that append position, is one the member held
before the service read its log: the service applies it and sends nothing for it. A
service that starts again reads its member's log from position 0, and so answers no
session a second time for what a service before it answered. An entry appended while
no service ran beside its leader is answered by none: a client that needs an answer
waits for one no longer than it chooses, and sends again.

A service holds its sessions' publications from the first entry it answers, and
gives them back at the first entry it does not answer, and a session's at its
session close event.

A reply that the session's publication does not take, because the client's stream is
not connected yet or is back-pressured, is tried again: the service leaves the entry
unread, by a controlled read (`spec/log-buffer.md`, "Reading"), and reads it again. A
service waits so for one session no longer than the keepalive interval ("Values"),
which is how often a client reads its session at the least. Once a session's
publication has taken nothing for that long since it first refused a reply, the
service drops the reply, counts it in `cluster.replies_dropped` and reads on, and it
drops each later reply to that session at once, counted, until the publication takes
one. A reply is dropped and counted too when the driver refused the session's
response channel, or the stream to the client has ended. So a client that does not
read its replies delays the replies to the other sessions by the keepalive interval,
once, and never the log: a member appends and commits whatever its service does.

An entry that is read again is answered again, from its first reply. A service
offers a reply to its session once, however often it reads the entry. For each
session it counts the replies that its readings of the entry have settled, sent or
dropped, which are the first replies the entry makes that session, and it does not
offer those again. When a reply to a session is refused, the replies that the same
reading makes that session after it are not offered either, so that the service's
replies to a session keep their order on its publication. A reply that is longer than one frame is refused before
it is counted, at every reading, and is not one of those replies. So no session is
sent a reply twice. The rule rests on what the replay check asks of every service:
the same replies, in the same order, each time an entry is read.

### Timers

A service's program schedules a timer and cancels one while it takes an entry, and
when a timer falls due the leader appends a timer event, which every member's
service reads at one position. In Aeron, these are a service's `scheduleTimer` and
`cancelTimer`, the consensus module's timer service and `onTimerEvent`. What is
handed to a program is decided by the service from the log alone, and the member
is an alarm clock whose mistakes cost an entry and no more (ADR 0151).

**The service's timers.** A service holds the timers that are pending: for each a
correlation id, any `i64` its program chose, and a deadline in the cluster's time
("Values"). A program schedules and cancels from inside its handling of an entry,
of any kind, and the service refuses the call anywhere else: the timers are then a
function of the log and of nothing else, as the program's state is, and are the
same beside every member at every position. Scheduling an id sets that timer's
deadline, whether the id was pending or not. Cancelling an id removes it, and
answers the program nothing, whether it was pending or not. A service holds at
most the timers of "Values": a schedule of an id that is not pending, when that
many are, is refused to the program and changes nothing. The refusal is by the
count of pending timers alone, so it is the same beside every member at the same
entry. A deadline at or before the time of the entry being taken is no error: the
timer is due at once. A service records a timer and answers its program alike
whatever member is beside it, whether one is, and whatever its driver answers.

A program whose handler does not take an entry (`spec/c-abi.md`, "Functions", the
cluster service) is handed the entry again, and every timer is then as it was
before that handling, whatever the handler scheduled and cancelled in it: a
handling that is not taken changes no timer and tells the member nothing. So the
next handling of the entry makes the same calls and is given the same answers, a
refusal at the bound among them.

A program that needs a timer from its beginning schedules it at the first entry it
is handed: every log begins with a new leadership term event at position 0, which
a service that reads from position 0 is handed first, and a service that starts
from a snapshot has its timers from the snapshot ("Snapshots", "A service's
start"). A timer that repeats is scheduled again by the handling of its own event.
Each event is an entry of the log, 64 bytes of it: a program that schedules a
deadline at or before the event's timestamp from each handling is handed the next
event as soon as its leader has been told and the event is committed and read, and
the log grows by an event each time for as long as the program does so.

**A timer event.** A service that reads a timer event hands it to its program when
the timer of that correlation id is pending with a deadline at or before the
event's timestamp. Until the program has taken the event the timer is in the
service's table, and counts toward the bound. Once the program has taken it the
timer is pending no more, unless the last call the program made for that id while
it handled the event was a schedule: the timer is then pending with that
schedule's deadline. Every other timer event is passed over: one whose id is not
pending, and one whose id is pending with a deadline after the event's timestamp.
The service hands its program nothing for it, changes no timer, and counts it in
`cluster.service_timer_events_passed_over`.

So a pending timer is handed to a program by the first timer event of its id whose
timestamp is at or after its deadline, and by no other: never with a timestamp
below its deadline, never after its cancel, never a second time unless it was
scheduled again, and at one position beside every member, whatever timer events
the log holds and whoever appended them. A schedule of a pending id sets its
deadline and makes no second timer. A program that does not take the event is
handed the same event again, which is the same handing.

**What a service tells its member.** The service holds one publication on
`cavalry:ipc` and the consensus module stream, the one its service acks are on
("Snapshots"), from its first open of its member's log to its close. It adds its
counters and then that publication in the read that first opens the log, before
that read hands its program an entry, whatever the program does: adding a
publication is the one command of a service's that takes long, since the driver
makes the whole of a new log resident, and no read that has a timer to tell
waits for it (ADR 0168). What it has to tell is a timer that an entry it took left pending, a
cancel, or the timers of the snapshot it started from, which it has to tell when
it has read its history, though it reads no entry. A driver that refuses that
publication, or the counters the service adds before it ("Snapshots", "A
service's snapshot"), fails no call that the program makes in a handler and
changes no timer: the service reports the refusal to its program as the error of
that read, once the read's entries are taken, and adds what is missing at each
read after it, before the read's entries (ADR 0159, ADR 0168). Until it has the
publication it tells its member nothing, and keeps what it has to tell.

On that publication the service tells the member on its driver what its timers
are. What it tells follows from the entries its program has taken, and from
nothing a handler did in a handling that was not taken. Two rules are all that
tell a member of a timer:

1. After an entry is taken, what the entry changed: a cancel timer for each timer
   that was pending before the entry and is not after it; and a schedule timer for
   each timer that is pending after it and was not before it, or was with another
   deadline. A timer whose event the entry was, and that the program scheduled
   again, is one of these whatever its deadline, the one it had included.
2. A schedule timer for every pending timer, again: when the service has read its
   history, below, and from then at its first read after each timer refresh
   interval of "Values", by its own monotonic clock.

Each message is sent when the stream takes it, and no reading of the log is held
back for one. A schedule timer carries the deadline the timer has when the message
is sent, and none is sent for a timer that is pending no more. A message the
stream does not take is sent at a later read, and a timer that waits to be sent
when rule 2 names it again is sent once. The service sends every cancel timer it
holds, the oldest first, before any schedule timer: a cancel it holds is of an
entry before the last one that scheduled its id, so for each id the member is told
in the order of the entries. A service holds at most the unsent cancels of
"Values", drops the oldest for the next, and counts it in
`cluster.service_timer_cancels_dropped`. So one read sends at most the unsent
cancels and the service's timers of "Values" together, 2,048 messages, each once;
it stops at the first that the stream does not take, and that one and those after
it wait for a later read, the cancels before the schedule timers again. A timer
event that a service passes over tells its member nothing.

Rule 1 tells a member soon, and rule 2 is what the member's table rests on: a
timer that a service holds and the member beside it lacks, or holds with another
deadline or with a mark that no event answers ("The leader's timer events"), is
put right within one timer refresh interval of the service's reading, whatever
made it so. A member that started again, a member that removed every timer, a
cancel that crossed a schedule, an event that a later leader's log does not hold:
none has a rule of its own.

While a service reads history ("Replies": an entry of a term below the term the
service loaded when it first opened its member's log, or of that term and below
the append position it loaded then) it tells its member nothing, and holds no
cancel for it: what those entries scheduled and cancelled a service before it told
the member, or is long past. It has read its history at the first of two moments:
when it reads an entry that is no history; and when it has read to that append
position, which is at once for a service that opens the log at or past it. A
member whose log was cut below that position may hold no entry up to it for as
long as it follows. It leads only in a later term, whose first entry is no
history: a service beside a member that leads has read its history once it has
read that entry. The service then sends by rule 2, which for a
service that started from a snapshot is the snapshot's timers and what the
entries after it made of them.

One service beside a member tells it of timers in this version, the one that
takes its snapshots ("Snapshots"). Where a second process of the same program
reads the member's log, it holds the same timers at the same position and tells
the member by the same rules at its own pace, so the member may be told a deadline
that the service ahead has replaced, or of a timer that is handed over already.
Which event is handed over does not change, since each service decides that from
the log; the member appends events that every service passes over, and a timer
may wait for the refresh of the service that is ahead.

A service does not know its member's version and sends the same to every member.
A member, its driver and its service are of one build ("Versions and the
upgrade"); a member of version 1 that were sent either message rejects it with the
reason `template` ("What a reader rejects"), counts it and acts on nothing.

**The member's timers.** A member reads schedule timer and cancel timer from the
consensus module stream as it reads service acks ("Snapshots", "The member's list
of a service's snapshots"), whatever its role. It keeps for each correlation id
the deadline last scheduled, and a cancel timer removes the id. It holds at most
the member's timers of "Values", twice a service's. A schedule timer for an id it
does not hold, with that many held, replaces the timer whose event the member
appended longest ago ("The leader's timer events"), which
`cluster.timers_replaced` counts. With no such timer the member
holds more than its service has pending, so some of what it holds is pending
nowhere, and it cannot tell which: it removes every timer, keeps the one it was
sent, and counts that in `cluster.timers_cleared`; its service sends the pending
ones again by rule 2. That is no error of any program. What a member holds is no
state it computes from the log: it is in no snapshot of the consensus module, a
member that starts holds no timer, and one holds what the service beside it has
told it since. `cluster.timers` holds how many.

**The leader's timer events.** A member that leads with its election closed
appends a timer event for a timer it holds whose deadline is at or before the
cluster's time, the milliseconds of its wall clock, the clock that stamps every
entry it appends ("Values"), with its term, the timer's correlation id and that
time as the timestamp: earliest deadline first, and of two that are due with one
deadline the lower correlation id first (ADR 0158), at most the timer events of
"Values" in a duty cycle, and one the log does not take is appended in a later
duty cycle. So an event's timestamp is never below the deadline its leader held.
The member keeps the timer, marked as one whose event it appended, and appends no
event for a timer so marked. A schedule timer that names the id takes the mark
off, whatever its deadline, and a cancel timer removes the timer with its mark.
So a timer that is pending in the leader's service a timer refresh interval after
its event was appended is given another event: the first is in no log that the
service reads, or the service has not read that far, or the event came before a
deadline the member had not been told. Every service passes over the event that
its timer does not answer. An appended event is counted in `cluster.timer_events`.
A member that does not lead appends none.

A member keeps a timer that is pending in no service when it was sent no cancel
for it, as when the cancel was dropped or its service stopped first: unmarked, it
is given one event when it is due and the member leads, which every service
passes over; marked, it stays until its place is needed. So `cluster.timers` may
be above `cluster.service_timers`.

A leader appends a timer event whatever the versions of the other members
("Versions and the upgrade"). Timers wait while no member leads, while the leader
is of a version before this one, and while no service runs beside the leader that
has told it of them, though a service runs beside every other member: `cavalry
doctor` on the leader's host says so. A leader whose service is behind its log, or
whose service was told of a timer event that a later leader's log does not hold,
may append an event for a timer that is cancelled, scheduled again for later, or
handed over already: every service passes that event over.

**Time.** A timer's deadline and an event's timestamp are the cluster's time, a
wall clock's, and never the monotonic clock that measures this page's timeouts
("Values"). A wall clock may step, and the next leader's may be behind the last
one's: the timestamps of the log's entries do not rise from entry to entry, and a
program assumes of time only what one entry tells it. A timer is due by the clock
of the member that leads when it is checked, so after a change to a leader whose
clock is behind, a timer waits until that clock reaches its deadline.

## The log's recordings

A member's consensus module writes one file in its driver's directory
(`spec/client-protocol.md`, "The driver directory"), `log-recordings-<member id>`, the
member's id in decimal: the recordings that hold the member's log, for the member's
service ("The service"; ADR 0103). The module writes the file whole under the name
`log-recordings-<member id>.tmp` and renames it over the file, so a reader reads one
whole file or the one before it; a reader opens the file once and reads at most
24,641 bytes of it, one byte more than the longest file, 40 + 1,025 × 24, so that a
file longer than that fails a check whatever it holds, `recordings-length` when the
checks before it pass ("What a reader rejects"); a file
the operating system will not let it open or read is the reader's error, which
names the file. The module writes it each time it names a recording, before it
stores the recording's id in `cluster.log_recording_id`, and removes it when it
closes, after it has stored −1 in `cluster.log_recording_id`, so that the file is
there for as long as the counter names a recording. A file the module cannot
write is its duty cycle's error, counted in
`cluster.log_recordings_unwritten` each cycle it fails, and the member stores −1 in
the counter until a later cycle has written the file. All integers are
little-endian.

The header, 40 bytes:

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the bytes `43 56 4C 59 4C 52 45 43` (`CVLYLREC`) |
| 8 | 4 | version | i32 | 0 |
| 12 | 4 | member id | i32 | 0 or more: the member whose log this is, the id in the file's name |
| 16 | 8 | live recording id | i64 | 0 or more: the recording the member records the live log into, the id it stores in `cluster.log_recording_id` next |
| 24 | 8 | live start position | i64 | 0 or more, a multiple of 32: where the live recording begins |
| 32 | 4 | entry count | i32 | 0 to 1025 |
| 36 | 4 | reserved | bytes | written as 0, never read |

Entry i is at 40 + i × 24, and the file's length is 40 + entry count × 24:

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | recording id | i64 | 0 or more |
| 8 | 8 | start position | i64 | 0 or more, a multiple of 32, at least the start position of the entry before |
| 16 | 8 | stop position | i64 | a multiple of 32, above the entry's start position |

The entries are the recordings that hold the member's log below its live recording,
in the order they begin: each recording the member stopped and keeps, at most 1,024
("Roles and the election", state 0), with the position it stopped at; and last,
while the member catches up (state 13), its recording of the replay, which is still
being made, with the position that recording had synced when the file was written,
at or above the live start position. A recording that holds nothing, its stop at its
start, has no entry. Two entries may overlap, and the live recording
may begin inside the last (state 13); the positions they share hold the same bytes.
A reader takes a position from the first entry whose start position is at or below
it and whose stop position is above it, and from the live recording when no entry
is so. A replay that follows an entry's recording ends where that recording stops,
at or above the entry's stop position, and the position it ends at is read by the
same rule.

## Counters

The implementing features define these in `counters.toml` and the reference page
generated from it (ADR 0052, features 3 to 5); this page names what each counts, so
that a reader of a counters file knows which transition it saw.

| Counter | What it counts |
| --- | --- |
| `cluster.role` | a gauge: 0 follower, 1 candidate, 2 leader |
| `cluster.election_state` | a gauge: the election state's code of "Roles and the election", 17 when no election runs |
| `cluster.leadership_term_id` | a gauge: the member's leadership term id, −1 before the first |
| `cluster.leader_member_id` | a gauge: the id of the member this member leads as or follows, −1 while it has none, which is in init, canvass, nominate and the two ballots: from the init of an election until it leads a term, on entering leader log replication, or takes a leader's new leadership term, on leaving canvass, nominate or a ballot for a follower's state; and again in the canvass a member goes back to when a follower log replication has ended, which begins no election and leaves `cluster.election_cause` as it is (ADR 0077, ADR 0102) |
| `cluster.election_cause` | a gauge: why the member's last election began, the election it is in or the one that closed last: 0 the member started; 1 it followed and its leader said nothing for the leader heartbeat timeout; 2 it followed and its image of the leader's log was no longer available; 3 it led and its active members fell below a quorum; 4 its election had closed and a message carried a term above its own; 5 its election before failed (ADR 0077, ADR 0102) |
| `cluster.commit_position` | a gauge: the commit position the member knows, the bound of its service's reading |
| `cluster.log_recording_id` | a gauge: the id of the recording the member records the live log into, −1 while it records none; the recording its service replays, or reads below through the member's file of recordings, which the member writes before it stores an id here ("The service", "The log's recordings"; ADR 0103) |
| `cluster.log_recordings_unwritten` | a duty cycle in which the member had a recording to name and could not write the file of its log's recordings, so that it stored −1 in `cluster.log_recording_id` ("The log's recordings"; ADR 0103) |
| `cluster.append_position` | a gauge: the member's append position |
| `cluster.elections` | an election begun, from init |
| `cluster.terms_led` | a term the member began to lead, on entering leader log replication: `cluster.role` going to leader |
| `cluster.leader_quorum_losses` | a leader whose active members fell below a quorum, once each time they fall: the election's leader holds an election, which in leader log replication and leader ready is its election failed, counted in `cluster.elections_failed` too (ADR 0082) |
| `cluster.log_images_lost` | a follower whose image of its leader's log was no longer available, so that it held an election |
| `cluster.elections_failed` | an election that failed in a state and began again at init |
| `cluster.elections_closed` | an election that closed: leader ready to closed, the term's first entry appended, or follower ready to closed, the append position sent |
| `cluster.election_steps` | a step of an election from one state to the next that neither begins it, fails it, closes it nor makes the member a leader: canvass to follower replay, follower replay to follower log init, follower log init to follower log await, follower log await to follower ready, leader log replication to leader replay, leader replay to leader init and leader init to leader ready; and canvass to nominate, and nominate, candidate ballot and follower ballot to follower replay, a candidate's step to follower replay also moving `cluster.role` from candidate to follower (ADR 0081); and canvass, nominate, candidate ballot and follower ballot to follower log replication, a candidate's step there moving `cluster.role` as its step to follower replay does, and follower log replication to canvass (ADR 0086); and follower replay to follower catch-up init, follower catch-up init to follower catch-up await, follower catch-up await to follower catch-up, follower catch-up to follower log init, and follower log init to follower ready for a member the catch-up left subscribed (ADR 0088) |
| `cluster.leader_heartbeat_timeouts` | a follower that held an election for want of a leader's heartbeat |
| `cluster.leader_step_downs` | a leader whose election had closed that held an election on a canvass position with either term, a request vote whose candidate term id or a commit position whose leadership term id above its own term |
| `cluster.higher_terms_seen` | a follower whose election had closed that held an election on a request vote whose candidate term id or a commit position whose leadership term id above its own term: a term the cluster began without it (ADR 0082) |
| `cluster.log_truncations` | a member that truncated its log on a new leadership term, to one of two positions: the message's next term base log position, the tail of an older term that no quorum recorded (ADR 0083); or, before it dropped the terms its leader's record lacks, the base of the lowest of them, the drop counted in `cluster.terms_dropped` (ADR 0096); counted once when it finishes, also a truncation the member owed after a fault, which finishes on a later duty cycle (ADR 0148) ("Messages during an election") |
| `cluster.truncations_refused` | a member told to truncate below the commit position it knows, which kept its log and failed its election ("Messages during an election"; ADR 0083) |
| `cluster.terms_dropped` | a member that dropped from its record of terms the terms its leader's record lacks, on a new leadership term whose log leadership term id was below its own and whose next leadership term id was above it ("Messages during an election"; ADR 0096) |
| `cluster.votes_for`, `cluster.votes_against` | a vote sent, by its value |
| `cluster.candidacies` | a ballot begun: a member in nominate that stored candidate term + 1, voted for itself and went to candidate ballot, `cluster.role` going to candidate (ADR 0081) |
| `cluster.ballots_lost` | a ballot that ended with no leader for the member, which canvasses again: a candidate on a vote against its candidate term or once it holds no vote of its own for that term, at once (ADR 0105), or at the election timeout short of a quorum, a member in follower ballot whose candidate's new leadership term did not come within the election timeout, or either that reads a new leadership term for a log term that is not its own, once it has dropped any terms the message says its leader's record lacks ("Messages during an election"; ADR 0096); two members that count one with `cluster.candidacies` each are a split vote (ADR 0081) |
| `cluster.leadership_terms_ignored` | a new leadership term ignored for its term: below the reader's candidate term in canvass, or not the candidate term in a ballot state; or read by a member that owes a truncation it could not then finish (ADR 0148) |
| `cluster.voters` | a gauge: the members to which the member's own string gives a vote, one bit for each member of the string in the rising order of their ids, the lowest id's bit the lowest, set for a voter: 7 for a string of four members whose highest id is a standby. It does not change while the member runs ("Standbys"; ADR 0134) |
| `cluster.standby` | a gauge: 1 when the member's own string marks it a standby, 0 when it gives it a vote. It does not change while the member runs ("Standbys") |
| `cluster.vote_floor` | a gauge: the member's vote floor, the leader's append position when it took that leader with its log empty, 0 included, and for as long as it holds which it withholds its vote and seeks no leadership; −1 for none. It changes once more after it is stored, to −1, when the member clears it, having joined a leader's live log since it stored the floor and in this run of its module with its append position at or past the floor and, from version 3, at or past a floor answer it took, counted in `cluster.vote_floors_cleared` ("A member that lost its log"; ADR 0145, ADR 0161, ADR 0163) |
| `cluster.vote_floors_cleared` | a vote floor the member cleared, having joined a leader's live log since it stored the floor and in this run of its module with its append position at or past the floor and, from version 3, at or past a floor answer it took, after which it votes as any voter does ("A member that lost its log"; ADR 0161, ADR 0163) |
| `cluster.floor_answers_sent` | from version 3, a floor answer the leader sent a member that holds a vote floor and asked, at most one each leader heartbeat interval for each member ("A member that lost its log", "The floor answer"; ADR 0163) |
| `cluster.floor_answers_ignored` | from version 3, a floor answer the member read and did not take: one that named another member, came from a member that is not its leader or for a term not its leader's, or named a session other than its own consensus publication's to that leader, as an answer to its earlier run does ("The floor answer"; ADR 0163) |
| `cluster.floor_answer` | a gauge, from version 3: the lowest log position of the floor answers the member took since it stored its vote floor, in this run, −1 for none; the clear's third condition reads it ("The floor answer"; ADR 0163) |
| `cluster.standby_vote_requests` | a request vote that the member read as a standby and answered with nothing: the candidate's string gives this member a vote, and the member's own does not ("Standbys", "Changing a standby's mark") |
| `cluster.votes_withheld` | a request vote that the member answered with nothing because it withholds its vote, its log empty while a member that holds one is heard or a vote floor held, and not every voter had reported a log the candidate's is at least, or a voter other than the candidate had named the request's term or a later one ("A member that lost its log"; ADR 0145) |
| `cluster.leaders_withheld` | a new leadership term that a voter whose log is empty ignored, where it would otherwise have taken the sender as its leader: it had not voted that leader in, and had not heard each other voter in this election name a term at most the leader's ("A member that lost its log", "Taking a leader"; ADR 0145) |
| `cluster.protocol_version` | a gauge: the member's own version of this page in semantic form, 256 for version 1. It does not change while the member runs. A member of a build older than this counter has none: every member of version 0, and one of the first build of version 1 ("Versions and the upgrade"; ADR 0137) |
| `cluster.sessions` | a gauge: open sessions, on every member, as the snapshot it started from and its log's committed session open and close events since left them |
| `cluster.sessions_accepted` | a session connect the leader accepted: a session id given, and a session open event to append |
| `cluster.sessions_opened` | a session opened by a committed session open event, on every member, or taken open from the snapshot the member started from ("A member's start"), so that this count less `cluster.sessions_closed` is `cluster.sessions` |
| `cluster.sessions_closed` | a session closed by a committed session close event, on every member, whatever its reason |
| `cluster.sessions_refused` | a session connect refused, with the version, the response channel or the limit as the detail |
| `cluster.connects_dropped` | a session connect dropped unanswered: the member's pending answers at their bound, the response channel refused by the driver, the answer unsent at the session connect timeout, or a connect the leader had accepted and gave up, its session open event not taken by the log for that timeout or not committed when the leader stopped leading |
| `cluster.ingress_ignored` | a session message, keepalive or close a member ignored: it does not lead, the term is not its own, or the session is not open or is being closed |
| `cluster.replies_dropped` | a reply the service beside the member dropped: the session's response channel had taken nothing for the keepalive interval since it first refused a reply, the driver refused the channel, or the stream to the client had ended ("The service", "Replies"; ADR 0118). The service adds the counter, as a client of the member's driver, when it first opens the member's log, keyed by the member id |
| `cluster.replies_stale` | a stale reply a client's session read and did not hand its caller: its leadership term id was below that of a reply the session had handed over ("Sessions", "Replies"; ADR 0121). The client adds the counter, on its own driver, when the leader has opened the session, keyed by the session id, an i64, and removes it when the session closes |
| `cluster.log_replications` | on the follower, a catch-up position sent from state 9, one for each replay it asks for (ADR 0092); on the leader, a replay started for a replication, once per replay where a replication spans several recordings |
| `cluster.log_replications_unavailable` | a replication whose append position did not move for the leader heartbeat timeout while below the stop position, so the member failed its election: the leader's recordings not holding the position asked, purged by retention; a replay that stopped for a fault; or a leader lost mid-replication |
| `cluster.app_version_mismatches` | a member that failed its election because the service's major version is not the one new leadership term carries |
| `cluster.join_position_mismatches` | an image of a replay or of the live log that joined somewhere other than where the member's log ends, so the member failed its election: in states 12 and 15 at a position other than its append position, in state 9 at a position other than the one it asked from (ADR 0086, ADR 0092) |
| `cluster.sessions_timed_out` | a session closed for the session timeout |
| `cluster.catchups` | a catch-up begun: on the follower, a catch-up position sent; on the leader, a replay started |
| `cluster.messages_rejected` | a message of this page a reader rejected, for any reason of "What a reader rejects"; and, on a member's ingress stream and in its read of its own committed log, a fragment of a message longer than one frame, a session message whose payload is longer than 1,344 bytes, and a session open or close event its table of sessions cannot hold ("Sessions") |
| `cluster.log_recordings_listed` | a list of the recordings of the member's log written to its log recordings file and synced: the list a creation writes, and each list after it, at the four moments of `spec/durable-log.md`, "The driver" ("The log"; ADR 0108) |
| `cluster.log_recordings_unsynced` | a duty cycle in which the member could not write or sync its log recordings file, so that the recording it had to list stayed out of its log and its append position did not count it; the next cycle writes again ("The log"; ADR 0108) |
| `cluster.log_recordings_forgotten` | a recording of its log that the member forgot, its table holding 1,024 stopped ones already: the member replays nothing below its oldest kept recording to a member or a service from then on, and starts again on its log directory only from a snapshot of a position at or above where its log then begins ("A member's start"; ADR 0091, ADR 0108, ADR 0130) |
| `cluster.truncations_finished` | a start that finished a truncation the member's stop had cut short: the last recordings its file names were gone from the catalog, or its record of terms held a term based above the end of its log ("A member's start"; ADR 0108) |
| `cluster.snapshot_position` | a gauge: the log position of the newest snapshot of its consensus module that the member's snapshots file lists, −1 while it lists none ("Snapshots"; ADR 0129) |
| `cluster.snapshot_actions` | a cluster action asking for a snapshot that the member appended while it led, at the snapshot interval, for its own process or for a control toggle ("Snapshots") |
| `cluster.snapshots_begun` | a snapshot the member began: it read the action and, in that duty cycle, started a recording of its one publication on the snapshot stream, which it added at its start ("Snapshots") |
| `cluster.snapshot_steps` | a snapshot whose messages the member had all written, so that it asked the recording's stop ("Snapshots") |
| `cluster.snapshots_taken` | a snapshot the member listed in its snapshots file, which counts from then on ("Snapshots") |
| `cluster.snapshots_failed` | a snapshot the member gave up, for one of the reasons of "A snapshot that fails", or whose list it could not write ("Snapshots") |
| `cluster.snapshot_start_position` | a gauge: the log position of the snapshot the member's start took its state from, where its read of its log began; −1 when it took none and read from position 0. It does not change while the member runs ("A member's start"; ADR 0130) |
| `cluster.snapshots_passed_over` | a snapshot that the member's start found listed and could not read whole, so that it took the one listed before it, or none ("A member's start") |
| `cluster.service_snapshot_position` | a gauge: the log position of the newest snapshot of a service that the member lists, −1 while it lists none ("Snapshots"; ADR 0131) |
| `cluster.service_snapshots_listed` | a service's snapshot that the member listed for a service ack ("Snapshots") |
| `cluster.service_acks_rejected` | a service ack for which the member listed nothing: a service id other than 0, a recording the catalog does not hold as a stopped recording of the service snapshot stream with bytes, a log position that is not above 0 or is above the commit position, or a position that is not a multiple of 32 or not above the newest the member lists for the service, which the snapshots file does not take ("Snapshots") |
| `cluster.service_snapshots_taken` | a snapshot the service beside the member wrote and sent its service ack for; the service adds the counter, with its member's id as the key ("Snapshots") |
| `cluster.service_snapshots_failed` | a snapshot the service beside the member gave up, for one of the reasons of "A service's snapshot"; the service adds the counter, with its member's id as the key |
| `cluster.service_snapshot_recording_id` | a gauge: the id, in the catalog of the member's driver, of the recording of the newest snapshot of a service that the member lists, −1 while it lists none; a service that starts from a snapshot reads it ("A service's start"; ADR 0132) |
| `cluster.log_purge_position` | a gauge: the position the member's log is needed from, the lower of the log positions of the newest snapshot of its consensus module and of the newest snapshot of a service that it lists, 0 while it lists no snapshot of one of the two; a recording of the log that stopped at or below it may be purged when the service's program asks it to start from its snapshot, and none beside a service whose program never asks ("A member's start") |
| `cluster.service_snapshot_start_position` | a gauge: the log position of the snapshot the service beside the member started from, −1 when the member named one and the service passed it over; the service adds the counter, with its member's id as the key, when its program asks it to start from a snapshot and the member names one ("A service's start") |
| `cluster.service_snapshots_passed_over` | a snapshot that the service beside the member was told of at its start and could not take, so that it reads the log from position 0; a refusal of its replay is one only when it is for the recording, error code 21 or 23, and any other refusal is the start's error and is not counted; the service adds the counter as the one before it ("A service's start") |
| `cluster.control_toggle` | a gauge that a command on the member's host adds, with the member's id and then the toggle's id as the key, and the command alone writes: 4, snapshot, asks the member for a snapshot, and every other value is no request of this version ("Snapshots", "The control toggle"; ADR 0147) |
| `cluster.control_toggle_taken` | a gauge: the id of the control toggle the member took last, for which it appended a cluster action while it led; −1 while it has taken none ("The control toggle") |
| `cluster.control_toggle_snapshot_position` | a gauge: the log position of the snapshot the member asked for the control toggle it took last, the position after the cluster action it appended; −1 while it has taken none. The command that asked waits for `cluster.snapshot_position` to reach it ("The control toggle") |
| `cluster.truncations_faulted` | a truncation of the member's log whose command or write faulted, which the member then owed until a truncation to its position finished; counted once for each truncation that becomes owed, and not for the attempts after it that fault ("Messages during an election"; ADR 0148) |
| `cluster.truncation_owed` | a gauge: the position of the truncation the member owes, −1 while it owes none: while it is at or above 0 the member does not nominate, reads a new leadership term only once the truncation has finished, and asks for the truncation again each duty cycle ("Messages during an election"; ADR 0148) |
| `cluster.timers` | a gauge: how many timers the member holds, each told to it by the service beside it in a schedule timer and not since removed ("The service", "Timers"; ADR 0151) |
| `cluster.timer_events` | a timer event the member appended while it led, for a timer it held whose deadline its clock had reached ("Timers") |
| `cluster.timers_replaced` | a schedule timer for which the member gave up the timer whose event it appended longest ago, its table at the member's timers of "Values"; where that timer is pending still its service sends it again ("Timers") |
| `cluster.timers_cleared` | a schedule timer at which the member removed every timer it held, its table at the member's timers of "Values" with no timer in it whose event the member had appended; it keeps the timer it was sent, and its service sends the pending ones again ("Timers") |
| `cluster.service_timers` | a gauge: how many timers are pending in the service beside the member; the service adds the counter, with its member's id as the key, when it adds its other counters ("Snapshots", "A service's snapshot"; "Timers") |
| `cluster.service_timer_events_passed_over` | a timer event the service beside the member read and did not hand to its program: its timer was not pending, or was pending with a later deadline; the service adds the counter, with its member's id as the key ("Timers") |
| `cluster.service_timer_cancels_dropped` | a cancel timer the service beside the member never sent, the consensus module stream having taken none while the unsent cancels of "Values" waited; the service adds the counter, with its member's id as the key ("Timers") |

## Values

| Value | This version |
| --- | --- |
| schema id, header version | 1, 0 |
| protocol version | 768, version 3 in semantic form, 0.3.0; a member reads every version whose major is 0 ("Versions and the upgrade") |
| stream ids | log 100, ingress 101, egress 102, log replay 103, service log 104, consensus module 105, service snapshot 106, snapshot 107, consensus 108, service snapshot replay 109, which is none of Aeron's. In Aeron 104 is the service stream, which carries the consensus module's messages to a service; here it carries the log a service reads ("The service") |
| members | at most 8, at least one of them a voter; a ninth is `members-count`, and a string of standbys alone `members-voters` |
| member id | 0 to 2147483647, unique |
| leader heartbeat interval | 200 ms: how often the leader sends commit position with nothing to report, and new leadership term during an election |
| leader heartbeat timeout | 10 s: a follower with nothing from its leader for this long holds an election; a member's append position older than this leaves it out of the quorum position, and a leader whose active members fall below a quorum holds an election, in states 5 and 8 as outside one; and the time a replication, a catch-up or a join may make no progress before the election fails |
| election timeout | 1 s: how long a candidate with no vote against it waits for the vote of a member it has heard, how long a voter waits for its candidate, and twice the longest random delay before a nomination |
| election status interval | 100 ms: how often canvass position is sent |
| startup canvass timeout | 60 s: how long a member whose first election since it started has not closed canvasses before a quorum candidate proceeds; at least twice the leader heartbeat timeout |
| session timeout | 10 s: a session with nothing from its client for this long is closed with the reason timeout |
| keepalive interval | 500 ms: how often a client sends session keepalive when it has sent nothing else, `spec/client-protocol.md`'s keepalive interval; and how long a service tries again a reply that a session's response channel does not take, before it drops it ("The service", "Replies") |
| session connect timeout | 5 s: how long a client waits for the session event answering its connect |
| sessions | at most 10 open or connecting; an eleventh connect is refused with `concurrent session limit` |
| pending answers | at most 10 redirects or refusals a member has yet to send; a connect beyond them is dropped unanswered and counted |
| live-add window | the lesser of term length ÷ 4 and 1,048,576 bytes: how close to the leader's position a catching-up follower adds its live subscription |
| cluster time | milliseconds since 1970-01-01T00:00:00Z, the leader's wall clock, time unit 0 |
| snapshot type id | 1, the consensus module's; 2, a service's |
| service id | 0: the one service beside a member that takes snapshots |
| service snapshot state | at most 1,073,741,824 bytes, 1 GiB, of a program's state in one snapshot, written 1,024 bytes to a message ("Snapshots") |
| snapshot interval | 268,435,456 bytes of the log, 256 MiB: how far past the last cluster action that asked for a snapshot a leader's log grows before the leader appends the next ("Snapshots") |
| snapshot timeout | 1 s: how long a snapshot being taken may go without its next step, a message its publication takes, its recording's stop announced or, for a service, its ack taken, before the member or the service gives it up ("Snapshots") |
| snapshot log sync wait | 2 s: how long a snapshot whose recording has stopped waits for the member's log to sync to the snapshot's log position before the member gives it up, the recording sync interval of `spec/client-protocol.md`, "Liveness and timeouts", within which the recorder syncs a recording by itself, and the snapshot timeout beside it for the sync ("Snapshots"; ADR 0143) |
| snapshot load timeout | 5 s: how long a member's start waits for its driver to play one snapshot's recording back before it passes that snapshot over, so 320 s at the most for the 64 snapshots a file lists ("A member's start") |
| service snapshot load timeout | 5 s: how long a service's start waits for the first message of its snapshot's recording, and for each message after one, before it passes the snapshot over ("A service's start") |
| control toggle look | at most 16 metadata records of the counters file, no more often than each millisecond: what a leader reads to find a control toggle, in a ring over the file's records, so a file of 16,384 records is read once in 1,024 looks, 1,024 ms at the soonest, and the largest, of 65,536, in 4,096; a leader whose duty cycle idles for longer than a millisecond looks once a cycle ("Snapshots", "The control toggle") |
| control toggle value | 4, snapshot, Aeron's `SNAPSHOT`; Aeron's other toggle states are out of this version |
| control toggle timeout | 10 s: how long the command that added a control toggle waits for the member to take it and to list the snapshot, before it removes the toggle and says what the member's counters hold |
| a service's timers | at most 1,024 pending in one service; a schedule of another id beyond them is refused to the program ("The service", "Timers") |
| unsent cancels | at most 1,024 cancel timers that a service holds for a consensus module stream that has not taken them; the oldest is dropped for the next and counted |
| the member's timers | at most 2,048 that a member holds, twice a service's; a schedule timer beyond them replaces a timer whose event the member appended, and with none the member removes every timer and is sent the pending ones again ("The service", "Timers") |
| timer refresh interval | 1 s: how often a service that has read its history sends its member every pending timer again, by the service's monotonic clock; what a member's timers lack or hold wrongly is put right within it |
| timer events | at most 10 that a leader appends in one duty cycle |

Timeouts are measured on a monotonic clock of the process that measures them, a
member's or a client's, nanoseconds that never step back, and on no wall clock: a step
of the host's clock moves no election. This departs from Aeron, whose cluster measures
its timeouts on its cluster clock, the wall clock by default, a step of which would
start or suppress an election (ADR 0058). The cluster's time, a wall clock's
milliseconds, is never used for a timeout.

## What a reader rejects

A reader is given one message of a stream of this page and runs these checks in this
order, reporting the first that fails by name; a rejected message is counted in
`cluster.messages_rejected` and nothing of it is acted on.

| Reason | Check |
| --- | --- |
| `too-short` | fewer than 8 bytes, or fewer than 8 + the header's block length |
| `schema` | the schema id is not 1 |
| `version` | the version is not 0; and, once the fields are read, between `length` and `enum`, a consensus message's `protocol version` field's major, its value shifted right by 16 bits arithmetic, is not 0, a negative value among those rejected; a session connect's `version` is read whatever its major, for the leader to refuse ("Sessions") |
| `template` | the template id is not one this version names: 1 to 6, 20 to 25, 31 to 33, 50 to 58, 100 or 102 to 105; 7 and 8 among those it does not, 31, 32 and 104 among those version 1 did not, and 58 among those version 2 did not |
| `stream` | the template is not one the stream carries: the log stream carries 1 and 20 to 25, the consensus stream 50 to 58, the ingress stream 1, 3, 4 and 5, the egress stream 1, 2 and 6, the consensus module stream 31 to 33, a snapshot 100 and 102 to 105 |
| `block-length` | the block length is not the template's |
| `length` | a variable field's length runs past the end of the message, or bytes remain after the last variable field of a template that has no payload |
| `enum` | a code, close reason, action, change type, mark, time unit, bool or vote holds a value its table does not name; a `flags` field is never rejected, its reserved bits ignored |
| `ascii` | an ASCII field holds a byte outside 0x20 to 0x7E |
| `member-id` | a leader, follower, candidate or changed member id is below 0 where its table allows no −1, or below −1 where it does |
| `position` | a log, term base, next term base, next log, commit or opened log position, a service ack's log position among them, is below 0 where its table allows no −1, or below −1 where it does |
| `term` | a term id is below −1 |

A members string is checked in this order, each check over the whole string: `members-grammar`
(an empty string; a member with fewer than six fields or more than seven, or with a
seventh that is not `standby`, those seven lower-case letters; an id that is not ASCII digits
making an integer from 0 to 2147483647; an endpoint without `:`, with an empty host, or
with a port that is not ASCII digits making an integer from 1 to 65535), `members-duplicate` (two members with one id),
`members-count` (more than 8 members), `members-voters` (every member a standby),
`members-self` (the member's own id is not in
the string); then `host` (a name that does not resolve), `spec/client-protocol.md`'s
error code 9. A host that holds `:` is an IPv6 literal and is written in square
brackets, else `members-grammar`.

A file of a member's log's recordings ("The log's recordings") is checked in this
order, and a reader that rejects it takes nothing from it: `recordings-too-short`
(fewer than 40 bytes), `recordings-magic` (the magic is not `CVLYLREC`),
`recordings-version` (the version is not 0), `recordings-member` (the member id is
below 0, or is not the id in the file's name), `recordings-length` (the entry count is
below 0 or above 1025, or the file's length is not 40 + entry count × 24),
`recordings-live` (the live recording id is below 0, or the live start position is
below 0 or not a multiple of 32), `recordings-entry` (an entry's recording id is below
0; its start position is below 0, not a multiple of 32, or below the start position
of the entry before it; or its stop position is not a multiple of 32 or not above its
start position).

## Vectors

`spec/vectors/cluster.json`, written by `spec/vectors/cluster.py`, the reference
encoder. Each vector has a `kind`:

- `message`: the `stream`, the `bytes` of one message and the `fields` a conforming
  reader decodes, a `payload` or variable field given as hex, or the `error` it
  reports; every template once valid, with its null values where a field allows them,
  and one vector per rejection in the order the checks run. The four messages that
  carry a version are each given twice more, as a writer of version 0 wrote it and
  as a writer of a later minor of major 0 would, and a conforming reader of this
  version reads both ("Versions and the upgrade").
- `members`: a members `string` and the `members` it parses to, each an id and its
  five endpoints as host and port, with `standby`, true, for a member the string
  marks a standby and no such field for a voter, or the `error`.
- `arithmetic`: `inputs` and the `fields` that follow: the quorum of a member count;
  the quorum position and the commit position from the members' append positions, the
  terms they were reported for, their ages, and the leader's own position, term and
  term base; whether one log is at least another; whether a candidate
  wins a ballot, at once or at the election timeout, or has lost it to a vote against its candidate term, from its candidate term, the
  votes with their terms and the members it has heard; whether a canvassing member is a unanimous or a
  quorum candidate from the members' reports; the stop position of a follower's log
  replication from its append position and the next term's base and end positions;
  what a reader of new leadership term does with its log, ignore, truncate, replicate,
  replay, wait or fail, and the term it records, from the message's fields, its own,
  its candidate term and whether it is in canvass; what a reader answers a request
  vote, for, against or nothing, and its candidate term after it, from its candidate
  term, log and state and the request's fields; what a leader does with a canvass
  position, answer with new leadership term, hold an election or update its table
  alone, from its term, its election state and the message's two terms; the
  next fields a leader writes into new leadership term for a reported term, from its
  record of terms; which of the leader's
  recordings a replay begins in and where it ends, from the leader's terms and the
  position asked; which terms a follower records when its replication reaches its
  stop, from the term replicated and its base, the stop position and the leader's
  term and base; the position after an entry from the position it begins at, its
  length and the MTU length.
- `timers`: the `steps` a service takes in order, and what it tells its member.
  A step has an `op`. `entry` is an entry that is no timer event, with the `calls`
  its handler makes, each a `schedule` of an `id` with a `deadline` or a `cancel`
  of an `id`, and `taken` false when the handler does not take it. `event` is a
  timer event with an `id` and a `timestamp`, and the `calls` and `taken` of its
  handler when it is handed over. `fill` is an entry taken for each id from 0 to
  its `count` less one, in that order, each with one schedule of the id for the
  step's `deadline` plus the id. `empty` is one entry taken that cancels each id
  from 0 to its `count` less one. `history` is the service beginning to read
  history, `history-ends` its end, and `refresh` a timer refresh interval that
  has passed. A step with `stream` false is one after which the consensus module
  stream takes nothing. For each step the `fields` that follow: `refused`, for
  each schedule in order whether it was refused, or how many of a fill's were;
  `handed`, whether an event was handed to the program; `pending`, the timers in
  the rising order of their ids, each an id and a deadline, or `pending_count`
  when more than eight are pending; `words`, what the stream took after the step,
  in order, each a `cancel` of an id or a `schedule` of an id with a deadline, or,
  when more than eight, `words_count` and the first two as `words_first`, and
  neither for a step with `stream` false; and `cancels_dropped`, how many cancels
  the step dropped, when it dropped any. Three vectors are at the bound of a
  service's timers, and one at the bound of its unsent cancels.
- `log recordings`: the `bytes` of a file of a member's log's recordings, the `member`
  id in its name, and the `fields` a conforming reader decodes, the member id, the
  live recording id, the live start position and the `entries`, each a recording id,
  a start position and a stop position, with `reads`, pairs of a position and the id
  of the recording the position is read from; or the `error` it reports, one vector
  per rejection in the order the checks run.

A conforming implementation decodes every valid vector to its fields, encodes those
fields to the same bytes, parses every members string, computes every arithmetic
vector's fields from its inputs, takes every step of a timers vector to its
fields, answers every read of a file of recordings, and
reports the named reason for every invalid one.

## Out of this version

Reserved, with their Aeron template ids, for the version that needs them: the
challenge and challenge response (7, 8), Aeron's authentication; the rest of the
service control protocol of a service in a process of its own, close session,
join log and the service termination position (30, 40, 42) and the admin request
and response (26, 27); dynamic membership, the cluster members
query and response, add passive member, cluster members change, join cluster, remove
member and the snapshot recording messages (34, 35, 41, 43, 70 to 74); termination
(75, 76); backup, heartbeat and standby snapshot (77 to 81); and what a snapshot of
this version does not hold, the cluster's members and a service's pending
messages (106, 107). A member of this version appends no membership
change event, session close event with the reason service action, or cluster action
whose action is suspend or resume, and a reader accepts each. Of Aeron's control
toggle a member of this version acts on one state, snapshot, 4: inactive, neutral,
suspend, resume, shutdown, abort and standby snapshot (0 to 3 and 5 to 7) are no
request here, and a toggle is no counter the member allocates ("Snapshots", "The
control toggle").
