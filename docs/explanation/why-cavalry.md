# Why Cavalry, and why not Aeron

This page answers: where is Aeron beatable, what does it get right that Cavalry must
keep, and why is the core Rust. It is the competitive brief the project started from,
written in October 2026. A designed, shareable version lives at
<https://claude.ai/artifact/73fiE4GQjXQQgo7UMWif7k>; this file is the source of truth.

## Thesis

Aeron is the fastest open transport you can get, and it is sold almost entirely to
capital markets by a consultancy that monetizes its difficulty. The gap is not raw
speed. It is *Aeron-class latency without Aeron-class expertise*. That product is
Cavalry: a single binary that is encrypted, observable, highly available and
Kubernetes-native in the open core, with first-class clients in every language, built
on a Rust core.

Head-on displacement at CME-class exchanges is unrealistic in year one. The beachhead
is every team that evaluated Aeron and chose Kafka, NATS or gRPC because Aeron was too
much to operate.

## Aeron in one table

| | |
| --- | --- |
| What it is | Reliable UDP unicast, UDP multicast and shared-memory IPC transport. Archive records streams for replay. Cluster runs Raft-replicated state machines on top. |
| Origin | Created by Martin Thompson and Todd Montgomery (Real Logic) in 2014. The team joined Adaptive Financial Consulting in 2022; Adaptive owns the trademark and the roadmap. |
| License | Apache-2.0 core. Premium components are proprietary. |
| Repository | 8.9k stars, 1.1k forks, 19,450 commits. Latest release 1.53.0, with breaking changes. |
| Published figures | Under 100 µs latency in the cloud, 18 µs on physical hardware, over 1M msg/s at microsecond latency. Aeron's own claims, not independently measured. |
| Clients | Java (first-class, the reference implementation), C and C++ in the main repo. .NET in a separate Adaptive repo. Rust exists only as community wrappers over the C client: `libaeron-sys`, `rusteron` (GSR), `aeron-glide` (cxx shim). No Python, TypeScript or Go. |
| Companion projects | Agrona (data structures), Simple Binary Encoding (SBE codecs). Both are part of the de facto stack. |
| Commercial layers | **Premium:** Cluster Standby, Transport Kernel Bypass (DPDK; earlier READMEs also list ef_vi, AWS DPDK, VMA), Transport Security (encryption), Insights (cluster health), Transport Selector, Data Retention Regulator, SBE Domain Mapper, SLA support. **Platform:** Aeron Sequencer, Aeron Hydra. **Accelerators:** Exchange Accelerator (matching engine, risk, market data, order entry). |
| Named users | CME Group, Coinbase, HSBC, SIX, Trade Republic, Bitvavo, Man, Galaxy, Talos, TransFICC, DriveWealth, Bullish (on Google Cloud). Every logo is capital markets. |
| Recent signal | 1.53.0 simplified event logging so a Java agent is no longer required, and added finer error handling for C/C++ Archive clients. Adaptive published seqcd, an etcd-compatible store built on Sequencer in two weeks with Claude, which shows where they want the platform story to go. |

## Openings: where Cavalry can be visibly better

Ranked by how clearly a buyer can see the difference in an evaluation, not by how hard
Aeron would find each one to fix. Each pairs the gap with the proof point to build into
the benchmark and demo.

### 1. It is a transport, not a bus. Everything above the wire is homework.

The gap:

- The API is channel URIs (`aeron:udp?endpoint=host:port`) plus integer stream IDs.
  No topics, no discovery, no request/reply, no schema registry, no consumer groups.
- Back-pressure is the application's problem: `offer()` returns `BACK_PRESSURED`,
  `NOT_CONNECTED`, `ADMIN_ACTION` and the caller writes the retry loop.
- A separate media driver process with idle strategies, term buffer sizing, MTU,
  socket buffers and a counters file that you read with `AeronStat`. Event logging
  needed a Java agent until 1.53.
- Adaptive sells training and consulting for exactly this. The learning curve is the
  business model.

The wedge and its proof:

- Working system in ten minutes: one binary, defaults that are correct on a laptop
  and on a c7i, topics and request/reply built in.
- A `doctor` command that explains why latency is bad (NUMA, hugepages, CPU governor,
  shm size) instead of a wiki page.
- Measure and publish: time to first message, lines of code for pub/sub plus
  request/reply in each language, number of settings touched to reach p99 under
  50 µs.

### 2. The things production needs are behind the Premium wall.

The gap:

- Encryption on the wire (Transport Security), kernel bypass, Cluster Standby,
  cluster health monitoring (Insights) and retention tooling are all paid add-ons.
- The open-source Cluster has no warm standby story and no first-class observability
  beyond counters.
- Pricing is opaque (contact sales), which stalls evaluations at mid-size firms.

The wedge and its proof:

- Encrypted, observable and highly available by default, in the open core. Charge
  for the managed control plane, compliance features and support instead.
- Ship the comparison matrix below in the README and keep it current. It is the
  single most persuasive page for a buyer who has already priced Premium.

### 3. Cloud and Kubernetes are an afterthought.

The gap:

- UDP multicast does not exist in AWS, GCP or Azure VPCs. Aeron's answer is
  multi-destination cast (MDC), which works but is configured by hand per endpoint.
- Shared memory in `/dev/shm`, hugepages and pinned busy-spin threads fight container
  CPU limits and default shm sizes. Aeron's own cloud number is about 5x its
  bare-metal number.
- No operator, no Helm chart, no OpenTelemetry export in the open project.

The wedge and its proof:

- A Kubernetes operator and Helm chart on day one. Pods that detect cgroup CPU
  limits and switch idle strategy instead of being throttled.
- OTel metrics, traces and structured logs from the driver itself.
- Prove it on EKS: one manifest, then publish p50 / p99 / p99.99 under CPU throttling
  against Aeron open source on the same nodes.

### 4. Java first, C second, everyone else on their own.

The gap:

- The Java client is the reference; C and C++ were reimplemented later and still lag
  on features and error handling (1.53 was still fixing C Archive error reporting).
- .NET is a separate repo. Rust is three competing community wrappers over the C
  client, each with its own build story. Python, TypeScript and Go have nothing
  official.
- Every client is a separate implementation of the protocol, so behaviour drifts.

The wedge and its proof:

- One Rust core exposed through a C ABI, with thin idiomatic clients for Python,
  Node, .NET, JVM and Go that are versioned and released together.
- A protocol conformance suite that every client passes in CI, published as a badge.

### 5. It is positioned as trading infrastructure and nothing else.

The gap:

- aeron.io calls itself "the global technology standard for trading systems."
  Marketing, case studies, accelerators and sales are all capital markets.
- Underserved by Aeron: defense and C2, industrial telemetry, robotics, gaming
  backends, ad exchanges, medical device fleets, any regulated enterprise that wants
  an ordered, durable, replayable event log with sub-millisecond latency on
  commodity cloud.
- Caveat: Zenoh (Eclipse) and iceoryx2 already attack robotics and automotive from
  Rust. Pick a vertical where neither they nor Aeron are strong.

The wedge and its proof:

- Lead with a non-finance reference deployment and a finance-grade benchmark. The
  combination is what Aeron cannot answer.
- Keep the finance door open: SBE compatibility and a FIX/SBE gateway sample cost
  little and remove an objection.

### 6. Day-two operations need a specialist.

The gap:

- Diagnosis means reading the CnC counters file, attaching spies, and knowing which
  of dozens of counters matters. Archive catalogs and replay are driven by code and
  low-level tools.
- Upgrades across Transport, Archive and Cluster have version coupling that the
  changelog warns about release by release.

The wedge and its proof:

- A built-in web console: streams, subscribers, loss, back-pressure, archive
  segments, cluster membership, all live.
- A replay CLI that answers "what did service X see between 09:30:00.000 and
  09:30:00.250" in one command.
- Single-binary upgrades with wire-protocol version negotiation, so mixed versions
  run during a rollout.

### Open core vs. Aeron, as the buyer would draw it

| Capability | Aeron open source | Aeron Premium | Cavalry, open core |
| --- | --- | --- | --- |
| Reliable unicast, multicast, IPC | Yes | Yes | Yes |
| Durable log and replay | Yes (Archive) | Yes | Yes |
| Raft replicated state machine | Yes (Cluster) | Yes | Yes |
| Wire encryption | No | Paid (Transport Security) | Default on |
| Warm standby / backup cluster | No | Paid (Cluster Standby) | Yes |
| Health and observability | Counters only | Paid (Insights) | OTel + console |
| Kernel bypass | No | Paid (DPDK) | AF_XDP in core; DPDK optional |
| Retention policy tooling | No | Paid | Yes |
| Topics, discovery, request/reply | No | No | Yes |
| Kubernetes operator | No | No | Yes |
| Official clients | Java, C, C++ (.NET separate) | Same | Rust, Python, Node, .NET, JVM, Go |

## Keep: strengths Cavalry must carry over

These are the reasons organizations choose Aeron. Dropping any of them turns Cavalry
into another broker.

**Brokerless, shared-memory architecture.** One media driver per host owns the
sockets. Clients map log buffers in shared memory; a publish is a write into a term
buffer plus a commit, and IPC and UDP are the same API. No broker hop, no extra
process in the latency path.
*Carry over:* same shape. One per-host daemon, memory-mapped log buffers, identical
API for local and remote streams. This architecture produces the latency numbers; a
broker cannot match it.

**Zero-copy, zero-allocation hot path.** `tryClaim()` hands the publisher a slice of
the log buffer to write into directly; the subscriber's fragment handler sees the
buffer in place. Nothing is allocated per message. SBE codecs encode straight into
that slice.
*Carry over:* claim/commit as the primary publish API, `&[u8]` views into mapped
memory on receive, no heap on the data plane. Offer SBE compatibility plus a native
zero-copy codec.

**Receiver-driven loss recovery and pluggable flow control.** Receivers detect gaps
and send NAKs; the sender retransmits from its own term buffer, so reliability costs
no extra storage. Status messages drive flow control, with max, min and tagged
strategies that decide how a multicast group handles a slow consumer.
*Carry over:* same NAK model and the same three flow-control policies, named the same
way, so Aeron engineers recognize them. Add a documented slow-consumer eviction
policy, which Aeron users build by hand today.

**A published, versioned wire protocol.** The transport protocol specification is
public, which is why independent clients exist at all and why GSR could build a Rust
wrapper for production trading.
*Carry over:* spec first, implementation second. Publish the spec with the
conformance suite and treat an unspecified behaviour as a bug.

**Archive plus Cluster is the real product.** A recorded, ordered input log feeding a
deterministic replicated state machine. Exchanges buy this, not the transport:
rebuild any service by replaying its log, fail over without losing an order.
Adaptive is productizing it as Sequencer and demonstrated it with an etcd
replacement.
*Carry over:* durable log and Raft replication are core components from the first
release, not a phase two. The deterministic-replay guarantee (same log in, same state
out) is the headline promise.

**Tail latency over mean, and the numbers are public.** Design decisions are judged
at p99.9 and beyond: busy-spin idle strategies, thread affinity, hugepages, bounded
queues. Aeron states its latency and throughput figures on the front page and has a
decade of production behind them.
*Carry over:* a benchmark harness in the repository from the first commit,
HdrHistogram output, results published per release against the same hardware. Never
quote a mean.

**Hot upgrades and automatic failover.** Cluster supports rolling upgrades and leader
failover without downtime, which is what 24/7 crypto venues such as Bitvavo cite when
they explain the choice.
*Carry over:* mixed-version clusters during rollout and sub-second leader election
are table stakes. Test both in CI with fault injection.

**Credibility: Apache-2.0, ten years, named logos.** The license is permissive, the
project is active, and the customer wall is a who's who of exchanges and banks.
Buyers trust it because other buyers already did.
*Carry over:* Apache-2.0 for the core with no relicensing clause. Design documents and
benchmark methodology in the open. One lighthouse customer before a public launch.

## Build: Rust as the core

Aeron is Java-first because in 2014 Java was the fastest runtime you could hire for,
and its tail-latency problems were handled with heroics: off-heap buffers,
zero-allocation discipline, warm-up loops, Azul Zing. Rust removes the need for the
heroics and clears the bar Aeron set.

### Why Rust clears the bar

- **No GC pauses, no JIT warm-up.** The p99.99 cliff that Java messaging systems
  engineer around does not exist. Cold start is the steady state.
- **Memory safety is a compliance feature.** A 24/7 daemon with a small, audited
  `unsafe` surface is an easier conversation with risk and security teams than a C++
  driver, and a different conversation from a JVM.
- **One core, every language.** A Rust core with an `extern "C"` ABI and cbindgen
  headers gives thin clients for Python (PyO3), Node (napi-rs), .NET (P/Invoke), JVM
  (Panama FFM) and Go (cgo), all sharing one protocol implementation. This inverts
  Aeron's model, where the C client had to re-implement the Java one.
- **The ecosystem now exists.** `io-uring`, AF_XDP bindings, `quinn` for QUIC,
  `zerocopy` and `bytemuck` for wire structs, `memmap2`, `crossbeam`,
  `hdrhistogram`, `opentelemetry`. None of this was available to Aeron's designers.
- **The buyers' own stacks are moving.** GSR runs Aeron through a Rust wrapper in
  production trading. A native Rust core meets that demand instead of wrapping Java's
  C port.

### Design rules for the core

- **Data plane is synchronous.** Dedicated, pinned threads with busy-spin, yield and
  backoff idle strategies, selectable at runtime. No async on the data plane. Tokio
  is fine for the control plane: admin API, discovery, metrics export.
- **Isolate the unsafe.** Cross-process ring buffers are inherently `unsafe`:
  `#[repr(C)]` layouts, explicit `Acquire`/`Release` atomics, a versioned layout
  header. Put them in one crate, fuzz it, test it under `loom`, run Miri where it
  can.
- **No locks on the hot path.** SPSC and MPSC lock-free queues, `#[repr(align(64))]`
  padding against false sharing, no `Arc<Mutex>` anywhere a message passes through.
- **Stable ABI only at the C boundary.** Rust-to-Rust integration goes through crates
  with semver. Everything else goes through the C ABI, which is the one surface
  frozen between releases.
- **Use the primitives Aeron predates.** io_uring for batched, zero-copy sends;
  AF_XDP as open-source kernel bypass, which undercuts Premium DPDK; QUIC for WAN and
  NAT traversal; eBPF for observability; cgroup v2 awareness so a throttled pod
  degrades gracefully.
- **Measure like Aeron does.** HdrHistogram at p50, p99, p99.9, p99.99 and max.
  Benchmarks in CI with the hardware named. A regression in p99.99 fails the build.

### Risks to plan for

- **Hiring.** The Rust pool is smaller than Java's. Mitigation: the Java-shaped
  client SDK is where most contributors will work, and it is thin.
- **Async gravity.** The ecosystem pulls toward Tokio everywhere. Hold the line on a
  synchronous data plane; this is the design decision that most determines tail
  latency.
- **FFI lifetimes.** Claim/commit over a C ABI means buffers whose lifetimes are
  enforced by convention, not the borrow checker. Make the misuse loud: debug builds
  poison released buffers.
- **Rust is not the differentiator.** Zenoh and iceoryx2 are already Rust. The
  differentiators are the durable log plus Raft story, the operator experience and
  the open core. Rust is how those get built without a GC in the way.

## Positioning: beachhead and proof

- **Do not lead with the exchanges.** Apache-2.0 plus a decade of hardening plus
  Adaptive's consultants make a CME-class displacement a multi-year effort. Win
  there last.
- **Lead with the teams that bounced off Aeron.** They chose Kafka, NATS or gRPC and
  pay for it in latency and in reconciliation bugs that an ordered, replayable log
  would have prevented.
- **Publish one benchmark page and keep it current:** Cavalry against Aeron open
  source on the same commodity cloud instances, reporting p50 / p99 / p99.99
  latency, messages per core, time to first message and number of settings touched.
- **Commercial model:** open core that includes encryption, standby and
  observability; revenue from a managed control plane, long-term support, compliance
  features (retention, audit, legal hold) and training that is optional rather than
  required.
- **Name the promise in one line:** Send in the cavalry. Aeron-class latency without
  Aeron-class expertise.

### Why the name

The cavalry is the fast unit of the army, and "send in the cavalry" means help
arrives quickly and handles the situation without the people on the ground having to
become experts. That is the product promise in two words. A charge is a formation,
not a mob, which covers ordered delivery, and mounted troops were the original
dispatch riders. It sits next to Brigade as a sibling name. The `cavalry` crate name
was unclaimed in October 2026 and no messaging, database or infrastructure product
used it; binary and CLI are `cavalry`.

## Sources

1. [aeron-io/aeron on GitHub](https://github.com/aeron-io/aeron): README, license,
   components, client list, repository statistics.
2. [aeron.io](https://aeron.io/): published latency and throughput figures, customer
   logos, 1.53.0 release note, seqcd announcement, Bitvavo case study.
3. [Aeron Enterprise & Support](https://aeron.io/enterprise-support/): Premium
   component list, Platform (Sequencer, Hydra), Exchange Accelerator.
4. [Aeron Transport Protocol Specification](https://github.com/aeron-io/aeron/wiki/Transport-Protocol-Specification)
   and [Flow and Congestion Control](https://github.com/aeron-io/aeron/wiki/Flow-and-Congestion-Control).
5. [rusteron-client](https://docs.rs/crate/rusteron-client/latest),
   [aeron-glide](https://docs.rs/crate/aeron-glide/latest/source/README.md),
   [libaeron-sys](https://docs.rs/crate/libaeron-sys/latest/source/README.md): the
   state of Rust bindings.
6. [Aeron.NET](https://github.com/AdaptiveConsulting/Aeron.NET): the separate .NET
   client.

Figures are Aeron's own published claims as of October 2026 and have not been
independently measured. Architecture descriptions are from the public documentation
and protocol specification. Rust crates are named as starting points, not
endorsements of a specific version.
