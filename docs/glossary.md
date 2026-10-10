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
| term | One fixed-size region of a log buffer. A stream's log is a ring of terms. | term | `docs/reference/log-buffers.md` |
| position | A byte offset into a stream's log, monotonically increasing across terms. | position | `docs/reference/log-buffers.md` |
| fragment | One wire-sized piece of a message larger than the MTU; the receiver reassembles fragments. | fragment | `docs/reference/protocol.md` |
| NAK | A negative acknowledgement sent by a receiver that detected a gap; the sender retransmits from its own term buffer. | NAK | `docs/reference/protocol.md` |
| status message | A receiver-to-sender message carrying the receiver's position and window, used for flow control. | status message | `docs/reference/protocol.md` |
| flow control (max, min, tagged) | The policy deciding how a sender treats a group of receivers: fastest receiver sets the pace (max), slowest does (min), or a tagged subset does (tagged). | flow control strategies | `docs/reference/flow-control.md` |
| back-pressure | The result code a publication returns when the receiver window or log buffer is full. The caller decides what to do; the driver never buffers more. | `BACK_PRESSURED` | `docs/reference/client.md` |
| idle strategy | What a data-plane thread does when it has no work: busy-spin, yield or backoff. | idle strategy | `docs/reference/driver.md` |
| counters file | The shared-memory file where the driver publishes every counter; the primary instrument for `doctor` and for OTel export. | CnC file / counters | `docs/reference/counters.md` |
| doctor | The `cavalry doctor` command: reads counters and host state and explains any latency or loss problem with a recommended action. | none (AeronStat shows counters; no diagnosis) | `docs/reference/cli.md` |
| log (durable) | The on-disk, ordered, replayable record of a stream, written by the driver. | Archive recording | `docs/reference/durable-log.md` |
| replay | Reading a durable log back into a subscription from a given position, at original or maximum speed. | Archive replay | `docs/reference/durable-log.md` |
| cluster | A set of drivers running the same replicated state machine over Raft, fed by a durable log. | Aeron Cluster | `docs/reference/cluster.md` |
| standby | A cluster member that follows the log without voting, ready to be promoted. Open core in Cavalry. | Cluster Standby (Premium) | `docs/reference/cluster.md` |
| data plane | Any code a message passes through between claim on the publisher and the fragment handler on the subscriber, including the driver's sender and receiver threads. | none | `CLAUDE.md` |
| control plane | Everything else: admin API, discovery, configuration, metrics export. May use async. | none | `CLAUDE.md` |
