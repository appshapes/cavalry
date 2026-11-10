# Cluster protocol, version 0

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

## Members and streams

A cluster is a members string, the same string on every member and every client:

```text
<id>,<ingress>,<consensus>,<log>,<catchup>,<archive>|<id>,<ingress>,...
```

Each member is six fields separated by `,`, members separated by `|`. `id` is the
member id, an integer from 0 upward, unique within the string; the other five are
endpoints, `host:port`, a host name or an address literal, an IPv6 literal in square
brackets, and a port from 1 to 65535, each resolved as `spec/udp.md`, "The channel
URI", resolves an endpoint. A string holds at most 8 members ("Values"). The endpoints
are where a member receives:

| Endpoint | Who receives on it | What |
| --- | --- | --- |
| `ingress` | the member, on the ingress stream | a client's session connect, session messages, keepalives and closes ("The session protocol") |
| `consensus` | the member, on the consensus stream | the consensus messages of every other member |
| `log` | the member, on the log stream | the leader's log; the leader's own `log` endpoint is the control address of its log publication |
| `catchup` | the member, on the log stream | the leader's replay of the log to a member that is behind, from the position it asked for |
| `archive` | the member | a peer's request to replicate its recording of the log; named here, used by no message of this version |

The streams are `cavalry:udp` streams, but for the service log, with these stream ids,
constants in this version:

| Stream | Stream id | Publication | Subscriptions |
| --- | --- | --- | --- |
| log | 100 | the leader's, `cavalry:udp?control=<its log endpoint>\|control-mode=manual\|fc=min`, every other member's `log` endpoint added as a destination (`spec/client-protocol.md`, "Commands", add destination), created at the term base log position for every term after the first, which needs a publication created at a position that no command of `spec/client-protocol.md` version 5 and no key of `spec/udp.md` version 1 gives: the version of those pages that adds one, Aeron's `init-term-id`, `term-id` and `term-offset` or their equal, is card 227's first change under its own ADR (ADR 0058); a member's recording of an image of this stream must begin at the image's join position, which `start recording` of version 5 does not give, since it records from the image's rebuild position when the command is handled and data that arrived before it lands in no recording: a recording armed on the subscription before its image arrives is the same change's second part | each other member's, `cavalry:udp?endpoint=<its log endpoint>`, recorded into its durable log; a member catching up, `cavalry:udp?endpoint=<its catchup endpoint>` |
| consensus | 108 | one per member per peer, `cavalry:udp?endpoint=<the peer's consensus endpoint>` | each member's, `cavalry:udp?endpoint=<its consensus endpoint>` |
| ingress | 101 | a client's, `cavalry:udp?endpoint=<a member's ingress endpoint>` | each member's, `cavalry:udp?endpoint=<its ingress endpoint>` |
| egress | 102 | the leader's, one per open session, and any member's, one per connect it answers with a redirect or a refusal, at most the pending answers of "Values" at a time, each on the client's response channel | the client's, on its response channel |
| service log | 104 | the member's driver's, one for each service: a replay of the member's recording of the log that follows it, on `cavalry:ipc` ("The service") | the service's, on `cavalry:ipc` |

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
| 6 | 2 | version | u16 | 0, this page's version |

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
and term ids and no other id ("What a reader rejects"). A `protocol version` or
`version` field holds this page's version in semantic form, major × 65536 + minor ×
256 + patch: 0 for version 0, the major being the value shifted right by 16 bits,
arithmetic, so that a negative value has a negative major; a reader rejects a
consensus message whose major it does not know, and reads a session connect's
whatever its major, so that the leader can answer it ("Sessions"). A `bool` is an `i32`, 0 false and 1 true. An enumeration is an
`i32` with the values its table names and no other; a `flags` field is read bit by
bit, each table naming its bits, and a bit it does not name is ignored.

## The log

The log is one stream of entries, each one message, written by the leader alone. Its
position is the stream's (`spec/log-buffer.md`, "Positions"): an entry's position is
the position after the last frame that carries it, the position at which it begins
plus the aligned length of each of its frames, 32 plus the fragment's payload rounded
up to a multiple of 32, with a message longer than MTU length − 32 bytes split into
fragments of that payload length and a last of the remainder. A member's log, in this
version, is its recordings of stream 100 taken in position order: one per term it
received live, from the term's publication with that term's session id, and one per
span it received from a replay ("Roles and the election", states 9 and 13), with the
replay's session id, since a recording holds one session id (`spec/durable-log.md`)
and each term's publication and each replay has its own. A member's append position
is the end of the contiguous span its recordings hold from the log's start, position
0: a position its recordings do not hold is a hole, and the append position stops at
the first hole whatever lies beyond it. A replay of the log takes each position from
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
record ("Messages during an election"). A member's
append position is the end
of the contiguous span its recordings hold, as "The log" defines it, never a
recording's position beyond a hole; the commit position is the position the leader has
found a quorum has appended ("Roles and the election"). An entry at or below the
commit position is committed and never changes; an entry above it may be truncated by
the next leader, and the service reads nothing above it ("The service").

The entries, each a template of this page:

| Template | Entry | Who appends it | When |
| --- | --- | --- | --- |
| 1 | session message | the leader | a session message arrived from an open session on the ingress stream; the header is rewritten with the leader's term and timestamp, the payload is the client's; or the leader's own process appended one of no session, cluster session id −1 ("The appointed leader") |
| 20 | timer event | the leader | a timer the service scheduled fell due; out of this version, since the service cannot schedule one ("Out of this version"), so no leader of this version appends it, and a reader accepts it |
| 21 | session open event | the leader | a session connect was accepted ("Sessions") |
| 22 | session close event | the leader | a session closed, with its reason |
| 23 | cluster action | the leader | the cluster suspends or resumes its ingress, or takes a snapshot |
| 24 | new leadership term event | the new leader | the first entry of every term |
| 25 | membership change event | the leader | a member joined or quit; out of this version, since membership is the members string, so no leader of this version appends it, and a reader accepts it |

### Session message header, template 1

Block length 24. On the log stream and the egress stream; on the ingress stream the
client writes it ("The session protocol").

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term the message belongs to |
| 16 | 8 | cluster session id | i64 | the session's, 0 or more; −1 on the log stream for a message of no session, which the leader's own process appended ("The appointed leader") |
| 24 | 8 | timestamp | i64 | the cluster's time, milliseconds since 1970-01-01T00:00:00Z, the leader's wall clock when it appended the entry; 0 on the ingress stream, where the client writes it and the leader replaces it |
| 32 | rest | payload | bytes | the service's message, 0 or more bytes |

### Timer event, template 20

Block length 24, no variable fields.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the term |
| 16 | 8 | correlation id | i64 | the timer's |
| 24 | 8 | timestamp | i64 | the cluster's time |

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
| 36 | 4 | protocol version | i32 | 0 |

### Request vote, template 51

Block length 32, no variable fields. A candidate's request to every other member.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | log leadership term id | i64 | the candidate's, as canvass position's |
| 16 | 8 | log position | i64 | the candidate's append position |
| 24 | 8 | candidate term id | i64 | the term the candidate proposes to lead, above every term it has known |
| 32 | 4 | candidate member id | i32 | the candidate's |
| 36 | 4 | protocol version | i32 | 0 |

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
| 8 | 8 | log leadership term id | i64 | the term the receiver reported, which this message describes |
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
| 92 | 4 | is startup | bool | 1 when the leader started with the cluster rather than taking over a running one |
| 96 | 8 | commit position | i64 | the leader's commit position |

### Append position, template 54

Block length 21, no variable fields. A follower's report to the leader, on every change
of its append position and at least every leader heartbeat interval.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the sender's log leadership term id, the highest term in its record, as canvass position's |
| 16 | 8 | log position | i64 | the follower's append position |
| 24 | 4 | follower member id | i32 | the follower's |
| 28 | 1 | flags | u8 | bit 0 (`0x01`) catch-up: the position was appended from the leader's catch-up replay; the other bits reserved, written 0, ignored |

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
as `host:port`. A follower's request to the leader for the log from a position.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the leader's term |
| 16 | 8 | log position | i64 | the position the follower wants the log from, its append position |
| 24 | 4 | follower member id | i32 | the follower's |

### Stop catch-up, template 57

Block length 12, no variable fields. The leader's word to one follower that its
catch-up replay has reached the leader's position and stops.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | leadership term id | i64 | the leader's term |
| 16 | 4 | follower member id | i32 | the follower's |

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
| 20 | 4 | version | i32 | the client's protocol version in semantic form, 0; a reader reads any major here, and the leader refuses one not its own ("Sessions") |

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
| 40 | 4 | version | i32 | the cluster's protocol version in semantic form, 0 |
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

## The snapshot marker

A snapshot is not a stream: it is a recording a member writes at a commit position and
reads back when it starts, whose shape is out of this version; a reader of this
version given a snapshot's messages, the marker alone in this version, reads them as
the `snapshot` stream of the vectors, with no stream id, and the snapshot stream id
107 stays reserved ("Values").

### Snapshot marker, template 100

Block length 40, no variable fields. The first and last message of a snapshot, and
the first of each of its sections; what lies between is out of this version
("Out of this version").

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 8 | 8 | type id | i64 | whose snapshot: 1 the consensus module's, the member's own state; a service's is its service id, out of this version |
| 16 | 8 | log position | i64 | the commit position the snapshot was taken at |
| 24 | 8 | leadership term id | i64 | the term at that position |
| 32 | 4 | index | i32 | the section's index, 0 for the begin and end markers |
| 36 | 4 | mark | i32 | 0 begin, 1 section, 2 end |
| 40 | 4 | time unit | i32 | 0, milliseconds |
| 44 | 4 | app version | i32 | the service's version in semantic form, 0 for none |

## Roles and the election

A member is a follower, a candidate or the leader. The leader writes the log and
commits it; a follower records the log and reports its append position; a candidate
is a follower asking for votes. A cluster of n members has a quorum of n ÷ 2 + 1,
integer division: 2 of 3, 3 of 5, 4 of 7, and 1 of 1.

**The commit position.** The leader keeps each member's last append position, the
term it was reported for and the time it arrived, its own among them, set to its own
append position and term on every duty cycle. A member is active while its last
append position is younger than the leader heartbeat timeout. A member counts toward
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
| 0 | init | consolidates its log: stops any replay or catch-up and leaves the live log, removing its subscription on its `log` endpoint and so stopping its recording of the leader's image there, so that the image it joins next is a new one whose join position state 15 checks; takes the higher of its stored candidate term and its leadership term as its candidate term. A cluster of one member goes to leader log replication as leader of candidate term + 1; every other member goes to canvass |
| 1 | canvass | as a follower, sends canvass position to every other member every election status interval; moves to nominate at once when every member has reported and no member's log is ahead of its own, a unanimous candidate, or when the canvass deadline has passed and the members that have reported with a log not ahead of its own, itself among them, make a quorum, a quorum candidate ("Logs compared"; the arithmetic vectors pin both). The deadline is the leader heartbeat timeout after the last word from a leader, or the startup canvass timeout after the state began when the member started with no term. A member that receives a request vote for a term above its candidate term while canvassing votes for it, when the candidate's log is at least its own, and goes to follower ballot, as "Messages during an election" says; one that receives new leadership term for a term at least its candidate term goes to follower replay or follower log replication as that message's rules say |
| 2 | nominate | waits a random delay up to half the election timeout, sending canvass position every election status interval, then takes candidate term + 1 as its candidate term, stores it before it sends anything, records its vote for itself and goes to candidate ballot |
| 3 | candidate ballot | as a candidate, sends request vote to every member it has not sent one to; becomes leader of the candidate term, going to leader log replication, when every member has voted for it for that term, or when the election timeout has passed and a quorum has, no member having voted against for that term, a vote for an older candidate term of its own counting for nothing ("Messages during an election"); goes back to canvass when the election timeout has passed and it has not become leader, short of a quorum or with a vote against, so that every member canvasses again at once. A request vote from another candidate for a higher term is answered as "Messages during an election" says: for, moving it to follower ballot, when that candidate's log is at least its own; against, taking the term, when that candidate's log is behind, after which it holds no vote for its candidate term, its own included, and goes back to canvass at the election timeout |
| 4 | follower ballot | waits for the candidate it voted for; goes to follower replay or follower log replication on its new leadership term, or back to canvass when the election timeout passes |
| 5 | leader log replication | as the leader, records its term with its append position as its base, before its own position counts, then adds its log publication at that position, the term base log position, which needs a publication created at a position ("Members and streams"); sends new leadership term and commit position every leader heartbeat interval; serves the catch-up positions of followers replicating what they lack (state 9, "Messages outside an election"); and waits until the quorum position reaches its append position, then goes to leader replay. It fails the election when its active members, those whose last append position is younger than the leader heartbeat timeout, fall below a quorum, and begins again at init |
| 6 | leader replay | replays its own log, its recordings in position order, from its log position to its append position into its service, then goes to leader init; a leader with nothing to replay goes at once |
| 7 | leader init | joins its log as leader for the term at its append position, the term recorded on entering state 5 ("The log"), and goes to leader ready |
| 8 | leader ready | sends new leadership term every leader heartbeat interval until a quorum of members counted as "The commit position" counts them, for the leader's term and at its log position, has reported within the leader heartbeat timeout, then appends the new leadership term event and closes the election |
| 9 | follower log replication | replicates what it lacks of the term its append position lies in, one term at a time: it subscribes to stream 100 on its `catchup` endpoint, as state 11 does, and sends catch-up position with its append position and that endpoint, counted in `cluster.log_replications`; the leader answers with replays of the recordings of its own that cover the span from that position to the stop position, the next term base log position when the append position is below it, else the next log position ("Messages outside an election"; the `replication-stop` vectors), one `start replay` after another in position order where the span crosses a recording's end, the next begun only once the follower's reported append position has reached where the last stopped, so that one replay image is on the endpoint at a time; the follower records each replay, an image of stream 100 on its `catchup` endpoint with the replay's session id, whose join position must be its append position, an image that joins elsewhere failing the election, counted in `cluster.join_position_mismatches`, reporting append position as it goes. Two timers bound it, Aeron's: while its append position is below the stop position, an append position that has not moved for the leader heartbeat timeout fails the election, counted in `cluster.log_replications_unavailable`, the leader's recordings not holding the span, purged by retention, or a replay that stopped for a fault being the causes; once its append position has reached a stop position that is the leader's term base log position it waits for the commit position it was told to reach its append position, a commit position or a new leadership term from its leader extending that wait by the leader heartbeat timeout, and a wait that runs out fails the election, counted in `cluster.elections_failed`; a stop below the base has no such wait, since the leader's commit position cannot rise until a quorum has recorded its term, which this follower cannot do before the next term. When the stop position is reached it records the term replicated with its base and, when the stop is the leader's term base log position however the stop was given, the next term base or the next log position, the leader's term at that base after it, both before it reports the stop, the `replication-record` vectors pinning both, so that it counts toward the leader's commits as a member that joined the term does; then it goes back to canvass, removing the subscription, a later new leadership term carrying it on to the next term |
| 10 | follower replay | replays its own log, its recordings in position order, from its log position to the lesser of its append position and the commit position it was told into its service, recording each term whose new leadership term event it replays, the leader's term already recorded at the base by the new leadership term that sent it here ("The log"), then goes to follower catch-up init when its append position is behind the leader's log position, else to follower log init; a replay that ends short of its append position goes back to canvass |
| 11 | follower catch-up init | subscribes to the log stream on its `catchup` endpoint and sends catch-up position with that endpoint; goes to follower catch-up await once sent, and fails the election when it cannot send within the leader heartbeat timeout |
| 12 | follower catch-up await | waits for the image of the leader's catch-up replay, the one image of stream 100 on its `catchup` endpoint, whatever its session id, since a replay's publication has a session id of its own (`spec/client-protocol.md`, "Commands", start replay), checks that the image's join position ("The log") is its append position, and goes to follower catch-up when it has joined it; an image that joins elsewhere, a stale replay's among them, fails the election, counted in `cluster.join_position_mismatches`, and so does no image within the leader heartbeat timeout |
| 13 | follower catch-up | records the catch-up replay, recording each term whose new leadership term event it reads, reporting its append position, the contiguous end, with the catch-up flag, and adds its `log` endpoint's live subscription once its append position is within the live-add window of the leader's, the lesser of term length ÷ 4 and 1,048,576 bytes; the live image's recording begins at the live recording's start, the position the follower joined the live log at, which lies at or past its append position, so that the two recordings overlap from there and the append position follows the replay until it reaches the live recording's start and the live recording from then; goes to follower log init once its append position has reached the live recording's start and the commit position it was told, and the leader has sent stop catch-up, which it sends when the follower's append position reaches its log position; fails the election when the catch-up makes no progress for the leader heartbeat timeout |
| 14 | follower log init | subscribes to the log stream on its `log` endpoint when it has not, and goes to follower log await; one already subscribed goes to follower ready |
| 15 | follower log await | waits for the live image by the log session id, checks that its join position is its append position, joins it there, records the leader's term at the leader's term base log position when its record lacks the term, which no path of this version into this state leaves lacking ("The log"), and goes to follower ready; an image that joins elsewhere, the leader having appended past the follower's position while it subscribed, fails the election, counted in `cluster.join_position_mismatches`, so that the member canvasses again and catches up, and so does no image within the leader heartbeat timeout |
| 16 | follower ready | sends append position to the leader and closes the election; fails it when it cannot send within the leader heartbeat timeout |
| 17 | closed | the election is over: the leader leads the term, a follower follows it |

**Messages during an election.** A member in init reads nothing. In every other state:
canvass position updates the sender's position in the reader's table, and a leader in
leader log replication or leader ready that reads one whose leadership term id, the
sender's candidate term, or log leadership term id is above its own term ends its
election in failure and begins again at init, counted in `cluster.elections_failed`,
while one in leader replay or leader init updates its table alone, the sender's next
canvass position reaching leader ready; a leader reads a canvass position whose log
leadership term id is below its own term, neither term above it, and answers with new
leadership term for that term, its next fields as the table gives them, the leader's term base log
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
vote for its new term and goes back to canvass at the election timeout, never leading
a term with votes cast for another; the `ballot` vectors carry the candidate's term
and each vote's. New
leadership term for the reader's candidate term, in follower ballot or candidate
ballot, or in canvass for a term at least its candidate term, is read against the
reader's log leadership term id, and one for any other term is ignored, counted in
`cluster.leadership_terms_ignored`, a term below the candidate term in canvass among
them, a departure from Aeron, which reads any term in canvass (ADR 0058), so that a member that has voted in a term follows no leader of
a lower one and never counts toward that leader's commits, Raft's rule: when the two
differ the reader goes back to canvass and takes neither the sender
as its leader nor the term; when they are the same the sender becomes the reader's
leader and the term its term, its candidate term too when the term is higher, in
memory alone ("The log"), and a
reader whose append position is above a next
term base log position the message carries, one not −1, truncates its log to that
position first, the uncommitted tail of an older term, `truncate recording` on the
recording that begins at or below the position and ends above it and `purge
recording` on every recording that begins above it, every recorded term whose base is
above the position leaving its record of terms with them, so that it votes as a
member of no term it no longer holds, and a message whose next term base log
position is −1 truncates nothing. A reader
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
is in canvass among their inputs, the term recorded among their fields. Append position is recorded by a candidate or leader against the follower.
Commit position from the reader's leader raises the commit position it was told.

**Messages outside an election, and the leader's in states 5 to 8.** A leader reads
append position and records it; catch-up position and starts a replay, a `start
replay` of a recording of its own on stream 100 to `cavalry:udp?endpoint=<the
follower's catchup endpoint>`, when the follower has none running, a replay counting
as running from its `replay ready` until its `replay stopped`
(`spec/client-protocol.md`, "Events"); a catch-up position that arrives while a replay
the leader has stopped still counts as running is kept, one per follower, the newest
replacing an older, and started when the `replay stopped` arrives. The replay begins at the position asked in the
recording that begins at or below that position and ends above it, the one that
begins first where two do, and the leader tells the two requests apart by the
position: one below the leader's term base log position is a replication (state 9),
whose replays end at the first term base log position in the leader's log above the
position asked, the leader starting the next recording's replay from the position
the last `replay stopped` carried, once that event came with reason 0 and the
follower's reported append position has reached the position, so that the follower's
image of each replay joins at its append position, a `replay stopped` with any other
reason starting nothing, which the follower's progress timer counts; each replay
started is counted in `cluster.log_replications`, once per replay where a
replication spans several recordings, and the `replay-end` vectors pin where a
replay begins and ends; one at
or above it is a catch-up within the leader's term (state 11), whose replay follows
the live recording, counted in `cluster.catchups`; a position the leader's recordings
do not hold, purged by retention (`spec/client-protocol.md`, "Commands", purge
segments), is answered by nothing, and the follower's timeout counts it. Each replay's
publication has a session id of its own in this version, so the follower's image of
it is on its `catchup` endpoint alone; and sends stop catch-up, stopping the replay with `stop replay`, when a
follower whose position was appended from the replay reports an append position at or
past the leader's log position. A leader, its election closed or in states 5 to 8, that
reads canvass position first stops any replay it has running to that follower with
`stop replay` and drops any next replay it was waiting to start for it, as a new
catch-up position from the follower does too, so that a follower that failed and
canvasses again never joins a stale replay or finds two started for one request;
during its election it then does with the message as "Messages during an election"
says; its election closed, it answers one whose leadership term id, the sender's
candidate term, and log leadership term id are both at or below its own term with new
leadership term for the reported log leadership term id, its next fields as the table
gives them, the three −1 when that term is its own, so that a restarted member finds
the leader and truncates no more than it must, and holds an election, beginning at
init, on one with either term above its own, Raft's step-down, counted in
`cluster.leader_step_downs`, a departure from Aeron, whose leader reads a canvass
position's log leadership term id alone (ADR 0058), so that a member whose candidate
term passed the leader's term, one that voted in a ballot the leader never saw,
rejoins through the election it brings instead of canvassing for as long as the
leader leads; the `canvass-position` vectors pin both. A follower that reads stop catch-up for its term
removes its catch-up subscription. Any member whose election has closed that reads
request vote whose candidate term id, or commit position whose leadership term id, is
above its own leadership term id holds an election, beginning at init, counted in
`cluster.leader_step_downs` when it was the leader; a leader in states 5 to 8 answers a
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

**The appointed leader.** Until the election is built (ADR 0060; ADR 0063; card 228),
no vote is taken: the member with the lowest id in the members string is the appointed
leader, the `appointed-leader` vectors, and no other member leads. No member enters
nominate, the ballot states or the replication and catch-up states, 2 to 4, 9 and 11
to 13, and none sends request vote, vote, catch-up position or stop catch-up; one of
those four that reaches a member is decoded and ignored. Every member of a cluster of
more than one goes from init to canvass and canvasses as the table says; a cluster of
one member leads from init, as the table says. A member that is not the appointed leader
stays in canvass, whatever has reported and whatever deadline has passed, until a new
leadership term moves it on. The appointed leader, where canvass would send it to
nominate, as a unanimous or a quorum candidate, goes to leader log replication
instead, as leader of candidate term + 1, which it stores first as nominate does, with
one condition more than canvass has: it does not lead while a member that has
reported holds a log ahead of its own ("Logs compared"), whatever the deadline. A
quorum candidate of the election leads without such a member and truncates it; this
version has no truncation, and a member ahead of an appointed leader, every member
having started with no term, holds a history the leader lost. So an appointed leader
that started again on an empty log directory canvasses and never leads, and commits
nothing by the positions of a log it does not hold; the `appointed-canvass` vectors
pin the rule. In Aeron, this is the appointed leader of
`aeron.cluster.appointed.leader.id`, which goes on from canvass through nominate and
a ballot the other members vote in; this version takes no vote, a departure card 228
ends (ADR 0063). It passes states 5 to 7 as any leader. In leader ready it waits for
more than a quorum, since no member of this version can catch up with a term whose
first entry it missed: it appends the new leadership term event and closes its
election at once when every member it has heard, itself among them, counts for its
term at its log position. A member is heard once the leader has had a canvass position
or an append position from it since its canvass began, a member that was silent
through the canvass and reports after it among them. A member counts when its last
report is an append position, which on this path a member sends only once it has
joined the leader's log (state 16), for the leader's term, at the leader's log
position and no other, and younger than the leader heartbeat timeout; a canvass
position counts for no member here, whatever term it names, since its sender has not
joined. With a quorum of the members counting, the leader closes once the leader
heartbeat timeout has passed since it entered leader ready, a member it heard that has
not joined its log by then being one that is not active. It closes with no fewer than a quorum, and not while the last report of any
member holds a log ahead of its own, a member that holds the history and was silent
through the canvass; the `leader-ready` vectors pin the rule. Each member the
appointed leader finds ahead, in canvass or in leader ready, is counted once in an
election in `cluster.appointed_leader_behind`. This
version has neither the catch-up nor the truncation by which a member joins a log it
does not already hold up to the leader's position, so a follower fails its election,
counted in `cluster.elections_failed`, and goes back to canvass where the election's
follower would truncate, replicate, wait or catch up: when its new leadership term
asks for a truncation, a replication (state 9) or a wait for a later message, and
when, after follower replay, its append position is behind the leader's log position
(state 11); it canvasses until a version that has the catch-up, one round every
election status interval, and the leader replicates to the others. A leader whose
active members fall below a quorum, counted in `cluster.leader_quorum_losses`, stays the leader
in the state it is in, leader log replication and leader ready among them, where the
election's leader fails its election: no election can replace it, its quorum position
is 0, and its commit position stays where it was ("The commit position"); and it
holds no election on a term above its own in a message, which no member of this
version holds. A follower that has had no commit position or new leadership term from
its leader for the leader heartbeat timeout counts `cluster.leader_heartbeat_timeouts`
and begins again at init, as any follower does, and so does one whose image of the
leader's log is gone ("The commit position"). A member starts with a record that holds
no term and a candidate term of −1; one whose record holds either does not start, and
says why and what to do: a restarted member needs the election to settle its term and
the catch-up or the truncation to settle its log, so until those exist a cluster is
started again as a new cluster, every member on a new log directory, never one member
alone on a new one. Before the sessions of "Sessions" exist, the leader's own process
appends for itself: once its election has closed it may append a session message
whose cluster session id is −1, no session's, with its term, the cluster's time and a
payload of its own, which a service reads as any session message and answers to no
session.

## Sessions

A client opens a session by sending session connect on the ingress stream to one
member's `ingress` endpoint, from a publication of its own, after subscribing to the
egress stream on its response channel. A follower answers with a session event of code
redirect, its leader member id and the ingress endpoints as its detail, and opens
nothing; a member with no leader answers the same with leader member id −1. The leader
answers: code error with the detail `invalid client version` when the connect's version
has a major other than its own; code error with the detail `invalid response channel`
when the response stream id is not 102 or the response channel is not
`cavalry:udp?endpoint=<host:port>`; code error with the detail `concurrent session
limit` when its sessions, open and connecting, are at the bound ("Values"); otherwise it
gives the session the next session id, appends a session open event, and once that
entry is committed sends the session event of code ok with the session id, the
correlation id and an empty detail. The session is open from then. Every refusal is
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
connect is dropped and never answered `invalid response channel`. A
connecting session whose ok event the leader cannot send within the session timeout
is closed as a timed-out session is, its session close event appended with the reason
timeout. A client that
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
later duty cycle. The client sends session
keepalive when the keepalive interval has passed since it last sent anything; a session
with nothing from its client for the session timeout is closed: the leader appends a
session close event with the reason timeout and sends a session event of code closed
with the detail `TIMEOUT`. A client closes its session with session close; the leader
appends a session close event with the reason client action and sends nothing. The
service's replies to a session are session messages on the egress stream, from the
leader alone ("The service"). When a term begins, its leader sends a new leader event
to every open session with the ingress endpoints, whether or not the leader changed,
since the clients' messages carry the term and the leader ignores one for another
term; the client sends its next message, a keepalive first, with the new term to the
new leader's `ingress` endpoint. The session ids and open sessions are the log's: a
new leader recovers them from the session open and close events it has replayed, so
that a session survives its leader, and counts each session's timeout from the close
of the election, not from the session's last message under the old leader. A
keepalive, a close or a session message for another term, or for a session that is
not open, is ignored and counts as no activity, each counted in
`cluster.ingress_ignored`.

## The service

The service is a client of its member's driver, through the client API and the C ABI,
as the consensus module is. It reads the member's own recording of the log through a
replay that follows the recording, `start replay` with a length of 2^63 − 1
(`spec/client-protocol.md`, "Commands"), into a `cavalry:ipc` subscription of its own
on the service log stream ("Members and streams"), from the position of an entry: 0,
or the position after the last entry it applied. The member names the recording in
`cluster.log_recording_id` ("Counters"), which is −1 while the member has none. The
path is the same on the leader and on a follower, and a service may start before its
member, with it or after it (ADR 0065). A service reads its own replay's session on
that stream and reads past any other, which is another service's.

A member keeps two rules of order for the service. It adds `cluster.log_recording_id`
and stores −1 in it before it adds `cluster.commit_position`, and a service reads the
recording's id only of a member whose `cluster.commit_position` it has found: a
counter holds 0 between its allocation and its first store, and 0 is a recording id.
And it stores another value in `cluster.log_recording_id` before it stores a commit
position that the recording does not hold.

The service reads entries up to the commit position and no further: it polls the
replay's image bounded by the member's `cluster.commit_position` ("Counters"), a read
that delivers no frame that begins at or past the bound (`spec/log-buffer.md`,
"Reading"), so that a service never reads an entry a quorum does not hold; and since a
replay of a live recording reads no further than the recording's synced position, it
never reads an entry its own member does not hold durably. The service takes a commit
position as its bound only when two checks hold after it has loaded it. The counter's
record is the member's: allocated, of the counter's type, and with the member id as
the first four bytes of its key (`spec/counters-file.md`, "Metadata record"). And
`cluster.log_recording_id`, loaded after the commit position, is the id of the
recording the service replays, from a record that is the member's by the same test.
From the first load at which either fails, because the member left the recording or
its consensus module closed and the driver freed its counters, the service reads the
counters no more and keeps the highest commit position it took. The bound never falls,
and neither a record the driver has given to another counter nor a commit position of
a later log is ever a bound. When the member's recording stops, the replay ends at the
stop position; the service has read no further than its bound, and it starts a replay
again, from its own position, when a member names a recording again.

It reads session messages, timer events,
session open and close events, cluster actions and new leadership term events in log
order, each entry once, with the cluster's time as each entry's timestamp field and
from no clock of its own, so that every member's service reaches the same state at
the same position, the replay check of invariant 1 (`CLAUDE.md`). The service of the
leader replies to a session with a session message on the egress stream; the service
of a follower reaches the same state and sends nothing. A service with a version
writes it as the app version field a leader announces, and a member whose service's
major version differs from the one new leadership term carries fails its election
from the state that read it, counted in `cluster.app_version_mismatches`, and begins
again at init, so that it never joins a log its service cannot apply; `doctor` names
the two versions.

## Counters

The implementing features define these in `counters.toml` and the reference page
generated from it (ADR 0052, features 3 to 5); this page names what each counts, so
that a reader of a counters file knows which transition it saw.

| Counter | What it counts |
| --- | --- |
| `cluster.role` | a gauge: 0 follower, 1 candidate, 2 leader |
| `cluster.election_state` | a gauge: the election state's code of "Roles and the election", 17 when no election runs |
| `cluster.leadership_term_id` | a gauge: the member's leadership term id, −1 before the first |
| `cluster.commit_position` | a gauge: the commit position the member knows, the bound of its service's reading |
| `cluster.log_recording_id` | a gauge: the id of the member's recording of the log, −1 while it has none; the recording its service replays ("The service") |
| `cluster.append_position` | a gauge: the member's append position |
| `cluster.elections` | an election begun, from init |
| `cluster.terms_led` | a term the member began to lead, on entering leader log replication: `cluster.role` going to leader |
| `cluster.leader_quorum_losses` | a leader whose active members fell below a quorum, once each time they fall: the election's leader holds an election, the appointed leader stays the leader and its commit position stops ("The appointed leader") |
| `cluster.appointed_leader_behind` | a member an appointed leader found holding a log ahead of its own, in canvass or in leader ready, once for each such member in an election: the leader does not lead, or does not close, while one reports so ("The appointed leader") |
| `cluster.log_images_lost` | a follower whose image of its leader's log was no longer available, so that it held an election |
| `cluster.elections_failed` | an election that failed in a state and began again at init |
| `cluster.leader_heartbeat_timeouts` | a follower that held an election for want of a leader's heartbeat |
| `cluster.leader_step_downs` | a leader whose election had closed that held an election on a canvass position with either term, a request vote whose candidate term id or a commit position whose leadership term id above its own term |
| `cluster.votes_for`, `cluster.votes_against` | a vote sent, by its value |
| `cluster.leadership_terms_ignored` | a new leadership term ignored for its term: below the reader's candidate term in canvass, or not the candidate term in a ballot state |
| `cluster.sessions` | a gauge: open sessions |
| `cluster.sessions_refused` | a session connect refused, with the version, the response channel or the limit as the detail |
| `cluster.connects_dropped` | a session connect dropped unanswered: the member's pending answers at their bound, the response channel refused by the driver, or the answer unsent at the session connect timeout |
| `cluster.ingress_ignored` | a session message, keepalive or close the leader ignored, for a term not its own or a session not open |
| `cluster.log_replications` | on the follower, a catch-up position sent from state 9; on the leader, a replay started for a replication, once per replay where a replication spans several recordings |
| `cluster.log_replications_unavailable` | a replication whose append position did not move for the leader heartbeat timeout while below the stop position, so the member failed its election: the leader's recordings not holding the position asked, purged by retention; a replay that stopped for a fault; or a leader lost mid-replication |
| `cluster.app_version_mismatches` | a member that failed its election because the service's major version is not the one new leadership term carries |
| `cluster.join_position_mismatches` | an image of a replay or of the live log that joined at a position other than the member's append position, so the member failed its election (states 9, 12 and 15) |
| `cluster.sessions_timed_out` | a session closed for the session timeout |
| `cluster.catchups` | a catch-up begun: on the follower, a catch-up position sent; on the leader, a replay started |
| `cluster.messages_rejected` | a message of this page a reader rejected, for any reason of "What a reader rejects" |

## Values

| Value | This version |
| --- | --- |
| schema id, version | 1, 0 |
| stream ids | log 100, ingress 101, egress 102, service log 104, consensus 108; 105 and 107 reserved for Aeron's consensus module and snapshot streams, out of this version. In Aeron 104 is the service stream, which carries the consensus module's messages to a service; here it carries the log a service reads ("The service") |
| members | at most 8; a ninth is `members-count` |
| member id | 0 to 2147483647, unique |
| leader heartbeat interval | 200 ms: how often the leader sends commit position with nothing to report, and new leadership term during an election |
| leader heartbeat timeout | 10 s: a follower with nothing from its leader for this long holds an election; a member's append position older than this leaves it out of the quorum position, and a leader whose active members fall below a quorum holds an election, in state 5 as outside one, or stays the leader when it is appointed; the time a replication, a catch-up or a join may make no progress before the election fails; and how long an appointed leader waits in leader ready for every member it has heard before it closes with a quorum ("The appointed leader") |
| election timeout | 1 s: how long a candidate waits for a quorum of votes, how long a voter waits for its candidate, and twice the longest random delay before a nomination |
| election status interval | 100 ms: how often canvass position is sent |
| startup canvass timeout | 60 s: how long a member that started with no term canvasses before a quorum candidate proceeds, the appointed leader among them ("The appointed leader"); at least twice the leader heartbeat timeout |
| session timeout | 10 s: a session with nothing from its client for this long is closed with the reason timeout |
| keepalive interval | 500 ms: how often a client sends session keepalive when it has sent nothing else, `spec/client-protocol.md`'s keepalive interval |
| session connect timeout | 5 s: how long a client waits for the session event answering its connect |
| sessions | at most 10 open or connecting; an eleventh connect is refused with `concurrent session limit` |
| pending answers | at most 10 redirects or refusals a member has yet to send; a connect beyond them is dropped unanswered and counted |
| live-add window | the lesser of term length ÷ 4 and 1,048,576 bytes: how close to the leader's position a catching-up follower adds its live subscription |
| cluster time | milliseconds since 1970-01-01T00:00:00Z, the leader's wall clock, time unit 0 |
| snapshot type id | 1, the consensus module's |

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
| `template` | the template id is not one this version names: 1 to 6, 20 to 25, 50 to 57 or 100; 7 and 8 among those it does not |
| `stream` | the template is not one the stream carries: the log stream carries 1 and 20 to 25, the consensus stream 50 to 57, the ingress stream 1, 3, 4 and 5, the egress stream 1, 2 and 6, a snapshot 100 |
| `block-length` | the block length is not the template's |
| `length` | a variable field's length runs past the end of the message, or bytes remain after the last variable field of a template that has no payload |
| `enum` | a code, close reason, action, change type, mark, time unit, bool or vote holds a value its table does not name; a `flags` field is never rejected, its reserved bits ignored |
| `ascii` | an ASCII field holds a byte outside 0x20 to 0x7E |
| `member-id` | a leader, follower, candidate or changed member id is below 0 where its table allows no −1, or below −1 where it does |
| `position` | a log, term base, next term base, next log or commit position is below 0 where its table allows no −1, or below −1 where it does |
| `term` | a term id is below −1 |

A members string is checked in this order, each check over the whole string: `members-grammar`
(an empty string; a member with other than six fields; an id that is not ASCII digits
making an integer from 0 to 2147483647; an endpoint without `:`, with an empty host, or
with a port that is not ASCII digits making an integer from 1 to 65535), `members-duplicate` (two members with one id),
`members-count` (more than 8 members), `members-self` (the member's own id is not in
the string); then `host` (a name that does not resolve), `spec/client-protocol.md`'s
error code 9. A host that holds `:` is an IPv6 literal and is written in square
brackets, else `members-grammar`.

## Vectors

`spec/vectors/cluster.json`, written by `spec/vectors/cluster.py`, the reference
encoder. Each vector has a `kind`:

- `message`: the `stream`, the `bytes` of one message and the `fields` a conforming
  reader decodes, a `payload` or variable field given as hex, or the `error` it
  reports; every template once valid, with its null values where a field allows them,
  and one vector per rejection in the order the checks run.
- `members`: a members `string` and the `members` it parses to, each an id and its
  five endpoints as host and port, or the `error`.
- `arithmetic`: `inputs` and the `fields` that follow: the quorum of a member count;
  the quorum position and the commit position from the members' append positions, the
  terms they were reported for, their ages, and the leader's own position, term and
  term base; whether one log is at least another; whether a candidate
  wins a ballot, unanimous or by quorum, from its candidate term and the votes with
  their terms; whether a canvassing member is a unanimous or a
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
  length and the MTU length; which member is the appointed leader, from the members'
  ids; whether the appointed leader leads from canvass and how many members are ahead
  of it, from the members' reports, its own log and whether its deadline has passed;
  whether an appointed leader in leader ready closes its election, from the member
  count, how many members it has heard since its canvass began, how many of those
  count for its term at its log position, how many are ahead of it and whether its
  deadline has passed.

A conforming implementation decodes every valid vector to its fields, encodes those
fields to the same bytes, parses every members string, computes every arithmetic
vector's fields from its inputs, and reports the named reason for every invalid one.

## Out of this version

Reserved, with their Aeron template ids, for the version that needs them: the
challenge and challenge response (7, 8), Aeron's authentication; the service control
protocol of a service in a process of its own, close session, schedule timer, cancel
timer, service ack, join log and the service termination position (30 to 33, 40, 42)
and the admin request and response (26, 27); dynamic membership, the cluster members
query and response, add passive member, cluster members change, join cluster, remove
member and the snapshot recording messages (34, 35, 41, 43, 70 to 74); termination
(75, 76); backup, heartbeat and standby snapshot (77 to 81); and the snapshot's
contents between its markers, the serialised sessions, timers, consensus module and
members (102 to 108), which the feature that writes the snapshot specifies. A member
of this version appends no timer event, membership change event or session close
event with the reason service action, and a reader accepts each.
