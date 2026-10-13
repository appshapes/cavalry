# Glossary

One row per term that appears in a public API, a counter name, a configuration key or
an error message. `just lint-docs` checks that every such term has a row. The Aeron
column exists so that an engineer coming from Aeron can map vocabulary in one place;
pages elsewhere mention Aeron at most once.

| Cavalry term | Definition | Aeron term | Reference |
| --- | --- | --- | --- |
| driver | The one daemon per host that owns the sockets and the shared-memory log buffers. Clients attach to it; it never sits in the message path as a broker. | media driver | `docs/reference/driver.md` |
| client | A process that attaches to the driver through shared memory and creates publications and subscriptions. | client | `docs/reference/client.md` |
| channel | A URI naming a transport endpoint, for example `cavalry:udp?endpoint=host:port` or `cavalry:ipc`. | channel | `docs/reference/channels.md` |
| stream | An ordered sequence of messages on a channel, identified by a stream id. | stream (stream id) | `docs/reference/channels.md` |
| publication | A client's handle for writing messages to one stream on one channel. | publication | `docs/reference/client.md` |
| subscription | A client's handle for reading messages from one stream on one channel, possibly from several publishers. | subscription | `docs/reference/client.md` |
| image | A subscription's view of one publisher's stream; one image per connected publication. | image | `docs/reference/client.md` |
| session | One publication's identity on a stream, so a subscription can tell publishers apart. | session (session id) | `docs/reference/protocol.md` |
| claim / commit | The zero-copy publish API: claim a slice of the log buffer, write into it, commit it. | `tryClaim` / `commit` | `docs/reference/client.md` |
| term | One fixed-size region of a log buffer. A stream's log is a ring of terms. | term | `docs/reference/log-buffer.md` |
| position | A byte offset into a stream's log, monotonically increasing across terms. | position | `docs/reference/log-buffer.md` |
| fragment | One wire-sized piece of a message larger than the MTU; the receiver reassembles fragments. | fragment | `docs/reference/protocol.md` |
| frame | The unit of a log buffer and of the wire: a 32-byte header, then a body, at a 32-byte-aligned offset. A data frame carries a fragment; a padding frame fills space. | frame | `docs/reference/frame.md` |
| padding frame | A frame of type 0 that fills the end of a term, or an abandoned claim. A subscriber skips it and delivers nothing. | padding frame | `docs/reference/frame.md` |
| term offset | The byte offset of a frame from the start of its term. | term offset | `docs/reference/frame.md` |
| reserved value | The 64-bit field of a data frame header that travels from publisher to subscriber unchanged, for the application's use. | reserved value | `docs/reference/frame.md` |
| end of stream (EOS) | The flag on a data frame saying the publication closed and nothing follows on its session. | EOS flag | `docs/reference/frame.md` |
| NAK | A negative acknowledgement sent by a receiver that detected a gap; the sender retransmits from its own term buffer. | NAK | `docs/reference/protocol.md` |
| status message | A receiver-to-sender message carrying the receiver's position and window, used for flow control. | status message | `docs/reference/protocol.md` |
| flow control (max, min, tagged) | The policy deciding how a sender treats a group of receivers: fastest receiver sets the pace (max), slowest does (min), or a tagged subset does (tagged). | flow control strategies | `docs/reference/flow-control.md` |
| back-pressure | The result code a publication returns when the receiver window or log buffer is full. The caller decides what to do; the driver never buffers more. | `BACK_PRESSURED` | `docs/reference/client.md` |
| idle strategy | What a data-plane thread does when it has no work: busy-spin, yield or backoff. | idle strategy | `docs/reference/driver.md` |
| counters file | The shared-memory file where the driver publishes every counter; the primary instrument for `doctor` and for OTel export. | CnC file / counters | `docs/reference/counters-file.md` |
| counter | One 64-bit value in the counters file, owned by one driver thread and moved by a state transition; its kind is `counter`, which never decreases, or `gauge`. Every counter is listed in `counters.toml`. | counter | `docs/reference/counters-file.md` |
| metadata record | The 256-byte description of one counter in the counters file: state, type id, key and label. | counter metadata record | `docs/reference/counters-file.md` |
| value slot | The 128-byte slot in the counters file that holds one counter's value and nothing else's. | counter value | `docs/reference/counters-file.md` |
| log buffer | The shared-memory file that carries one publication's stream: three term buffers and a metadata page. | log buffer | `docs/reference/log-buffer.md` |
| partition | One of the three term buffers of a log buffer; term count modulo 3 says which holds a term. | partition | `docs/reference/log-buffer.md` |
| tail counter | The 64-bit field per partition that packs the term id and the term offset of the next free byte; a claim advances it. | raw tail | `docs/reference/log-buffer.md` |
| active term count | How many terms the stream has rotated through; the partition of the active term is this modulo 3. | active term count | `docs/reference/log-buffer.md` |
| initial term id | The term id of the stream's first term, chosen by the driver; term count 0. | initial term id | `docs/reference/log-buffer.md` |
| correlation id | The id a client takes from the command ring's counter for each command, and the driver gives the publication or subscription it registers; names its log buffer file and keys its counters. | correlation id (registration id) | `docs/reference/log-buffer.md` |
| rotation | Moving a stream to the next partition when a term is full: the next tail and the active term count are compare-and-swapped, and the caller retries with `admin action`. | log rotation | `docs/reference/log-buffer.md` |
| publication window | How far a publisher may run ahead of the slowest subscriber; at most half a term. | term window length | `docs/reference/log-buffer.md` |
| publisher limit | The position a publisher may write up to: the slowest subscriber's position plus the publication window, a counter the driver maintains. | publisher limit (`pub-lmt`) | `docs/reference/log-buffer.md` |
| end of stream position | The metadata field the driver sets to the final position when a publication closes; a subscriber at or past it has seen everything. | end of stream position | `docs/reference/log-buffer.md` |
| unblock | The driver turning a claim that was never committed into a padding frame or padding record after a timeout, so the stream or the ring moves on. | unblock | `docs/reference/log-buffer.md`, `docs/reference/command-ring.md` |
| unblocked | The result a producer gets when its claim word is gone before it writes or its commit's compare-and-swap fails: the driver unblocked the claim, the command was not delivered, and the producer may claim again. | none; Agrona's ring commits with a store and the late commit lands | [Command ring](reference/command-ring.md) |
| admin action | The claim result that says a rotation happened and the caller retries. | `ADMIN_ACTION` | `docs/reference/log-buffer.md` |
| command ring | The shared-memory file through which every client sends the driver its commands: many producers, one consumer, records read in order and cleared. | to-driver command buffer (`ManyToOneRingBuffer`) | `docs/reference/command-ring.md` |
| broadcast buffer | The shared-memory file through which the driver sends every client its events: one transmitter, any number of receivers, and a receiver that falls a ring behind is lapped. | to-clients buffer (`BroadcastTransmitter`) | `docs/reference/broadcast-buffer.md` |
| record | The unit of a command ring or a broadcast buffer: an 8-byte header of length and message type, then the message, aligned to 8 bytes. A padding record fills the end of the ring or an abandoned claim. | record | `docs/reference/command-ring.md` |
| message type | The positive integer in a record's header that says which command or event the message is; −1 marks a padding record. | message type id | `docs/reference/command-ring.md` |
| tail position | The position in a ring at which the next record is claimed; producers advance it. | tail position (producer position) | `docs/reference/command-ring.md` |
| head position | The position in the command ring at which the driver reads the next record; it advances once the records read are cleared. | head position (consumer position) | `docs/reference/command-ring.md` |
| head cache | A copy of the head position that producers keep and consult before the head itself, so that a claim does not read the consumer's cache line every time. | head cache position | `docs/reference/command-ring.md` |
| consumer heartbeat | The clock the driver stores in the command ring on its timer, and −1 when it closes, so that a client can tell a stopped driver from a slow one. | consumer heartbeat time | `docs/reference/command-ring.md` |
| tail intent | The position the broadcast buffer's tail will reach once the record being written is complete; a receiver a whole ring behind it is lapped. | tail intent counter | `docs/reference/broadcast-buffer.md` |
| latest record | The position of the last complete record in the broadcast buffer, where a receiver joins and where a lapped receiver resumes. | latest counter | `docs/reference/broadcast-buffer.md` |
| lapped | The state of a broadcast receiver whose cursor is a whole ring behind the tail intent: records were overwritten before it read them, and it is told so. | lapped | `docs/reference/broadcast-buffer.md` |
| doctor | The `cavalry doctor` command: reads counters and host state and explains any latency or loss problem with a recommended action. | none (AeronStat shows counters; no diagnosis) | `docs/reference/cli.md` |
| log (durable) | The on-disk, ordered, replayable record of a stream, written by the driver. | Archive recording | `docs/reference/durable-log.md` |
| replay | Reading a durable log back into a subscription from a given position, at original or maximum speed. | Archive replay | `docs/reference/durable-log.md` |
| cluster | A set of drivers running the same replicated state machine over Raft, fed by a durable log. | Aeron Cluster | `docs/reference/cluster.md` |
| standby | A cluster member that follows the log without voting, ready to be promoted. Open core in Cavalry. | Cluster Standby (Premium) | `docs/reference/cluster.md` |
| data plane | Any code a message passes through between claim on the publisher and the fragment handler on the subscriber, including the driver's sender and receiver threads. | none | `CLAUDE.md` |
| control plane | Everything else: admin API, discovery, configuration, metrics export. May use async. | none | `CLAUDE.md` |
| client protocol | The commands a client sends the driver through the command ring and the events the driver answers with through the broadcast buffer, their type ids, fields and timeouts. | control protocol (`io.aeron.command`) | `docs/reference/client-protocol.md` |
| driver directory | The directory, `cavalry-<user>` under `/dev/shm` or the temporary directory, that holds one driver's counters file, command ring, broadcast buffer and log buffers; a client opens it to find the driver. | Aeron directory (`aeron.dir`) | `docs/reference/client-protocol.md` |
| client id | The id a client takes from the command ring's correlation id counter when it attaches and puts in every command; the driver tracks the client's liveness by it. | client id | `docs/reference/client-protocol.md` |
| registration id | The id under which the driver knows a client's publication or subscription: the correlation id of the command that added it. A concurrent publication shared by several registrations carries the first one's as the publication's registration id, which names its log and its images. | registration id | `docs/reference/client-protocol.md` |
| session id | The id the driver gives one publication's stream, random at start and one more per publication; with the stream id it names an image. | session id | `docs/reference/client-protocol.md` |
| stream id | The id a client gives a stream on a channel; a publication and a subscription with the same stream id and channel connect. | stream id | `docs/reference/client-protocol.md` |
| keepalive | The command a client sends every 500 ms when it has sent nothing else, so the driver knows it is alive. | client keepalive | `docs/reference/client-protocol.md` |
| client liveness timeout | How long, 10 s, the driver waits for a command from a client before it times the client out, removes its publications and subscriptions, and sends a client timeout event. | client liveness timeout | `docs/reference/client-protocol.md` |
| driver timeout | How long, 10 s, a client lets the consumer heartbeat go stale before it treats the driver as gone and closes. | driver timeout | `docs/reference/client-protocol.md` |
| publication linger | How long, 5 s, a closed publication's log stays for subscribers to drain before the driver removes it. | publication linger timeout | `docs/reference/client-protocol.md` |
| error code | The number in an error event that says why the driver refused a command, Aeron's codes. | error code | `docs/reference/client-protocol.md` |
| conductor | The driver's one control thread: on every duty cycle it stores its heartbeat, reads and answers commands, unblocks the ring, times out clients and keeps every publication's positions. | conductor (`DriverConductor`) | `docs/reference/client-protocol.md` |
| configuration key | A named setting of the driver, registered in `config.toml` with why its default cannot always be right and the `doctor` check that fires when it is set wrong; read from its flag, else its environment variable (`CAVALRY_` and the name upper-cased with dots as underscores), else its default. | system property (`aeron.*`) / `AERON_*` | `docs/reference/config.md` |
| SPSC queue | A bounded queue between two threads of one process, one producer and one consumer, with no lock: `offer` returns the item when it is full, `poll` returns nothing when it is empty. | `OneToOneConcurrentArrayQueue` (Agrona) | `crates/cavalry-shm/README.md` |
| client conductor | The thread a client starts when it attaches: it sends the keepalive, reads every event, completes the client's add and remove operations, and closes the client when the driver is gone or has timed it out. | client conductor (`ClientConductor`) | `docs/reference/client-protocol.md` |
| offer | The copying publish API: a message is copied into the log as one frame, or as fragments committed in order, against the publication limit; the result is the position after it, or the same refusals as a claim. | `offer` | `docs/reference/log-buffer.md` |
| fragment handler | The closure a subscription hands each fragment to on a poll: the body as a view into the log, and the frame's header. | `FragmentHandler` | `docs/reference/log-buffer.md` |
| recording | One stream captured to the durable log from a start position, identified by its recording id. | recording | `docs/reference/durable-log.md` |
| recording id | The index of a recording's slot in the catalog, never reused. | recording id | `docs/reference/durable-log.md` |
| catalog | The durable log's index of recordings: a header and one 1024-byte slot per recording. | catalog | `docs/reference/durable-log.md` |
| segment file | One file of a recording holding the log buffer's raw term bytes from a segment base position. | recording segment | `docs/reference/durable-log.md` |
| time index | A recording's list of (timestamp, position) entries, at most one per millisecond, that resolves an instant to a position. | none | `docs/reference/durable-log.md` |
