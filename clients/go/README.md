# cavalry, the Go client

## What this does

A Go module over the C ABI of `spec/c-abi.md`, generated from the committed header by
`clients/generate.py go`: the package `cavalry` (module
`github.com/appshapes/cavalry/clients/go`) reaches the ABI through cgo, compiled
against the header and linked with `cavalry-ffi`'s static library, and gives the
handles the forms a Go user expects. A `Client` closes with `Close`; a `Publication`
claims a slice of the log as a `[]byte` over it or offers a `[]byte`; a `Subscription`
polls with a fragment handler function that receives the body as a `[]byte` into the
log and the frame's `Header`; a `Recording` and a `Replay` wrap the driver's archive.
Every message the client sends or receives goes through one `extern "C"` call on the
caller's goroutine, as the ABI's page promises (ADR 0046's boundary; ADR 0051). The
module has no dependency beyond Go's standard library: cgo is part of the Go toolchain,
and the C compiler it needs is the one `cavalry-ffi`'s own build needs.

cgo's pointer rules decide what crosses the boundary (ADR 0046, the Go client). The ABI
keeps the `clientd` it is given for the length of a poll and hands it back to the
handler, and C must not hold a Go pointer, so the client passes the subscription's
index in a bounded table of 4,096 entries, and the trampoline reads the subscription
back from the table with one atomic load. A slice over the log is made with
`unsafe.Slice` from the address the ABI gives, which allocates nothing; a slice of the
caller's bytes is passed to `cavalry_publication_offer` as the address of its first
element, which cgo allows because the bytes hold no Go pointer and the ABI copies them
before it returns.

## What it must never do

- Never run a message through anything but one ABI call: no lock, no goroutine, no
  channel, no allocation, no copy of a fragment for the handler. `clients/lint.py`
  reads the per-message functions of `cavalry.go` and `trampoline.go`, the claim, the
  reserved value, the commit, the abort, the offer, the poll, the header's values and
  the trampoline, for Go's forms of each (`sync`, a lock or a wait through a variable
  or a field, such as `registryMu.Lock()`, which names no `sync.`, a `go` statement, a
  channel send or receive, `select`, `panic`, and `make`, `new` and `append`), and
  those of `shim.c`, the C trampoline and the poll, for C's (`pthread_mutex_*`,
  `pthread_rwlock_*`, `mtx_*`). `just lint` fails on any of them, in the function's
  own body or in a function of the package it calls.
- Never return an error for an `Offer` code: `TryClaim` and `Offer` return the position
  after the frame or the code below zero (`PublicationNotConnected`,
  `PublicationBackPressured`, `PublicationAdminAction`, `PublicationClosed`,
  `PublicationMaxPositionExceeded`, `PublicationError`), as the ABI does; the caller
  branches.
- Never swallow the handler's panic, or a fragment: a fragment handler that panics
  keeps every later fragment of that poll reaching the handler, and `Poll` returns a
  `*HandlerError` once the ABI has returned, with the first panic's value as its
  `Cause` and the counts. A panic must not unwind through the ABI's frames, so the
  trampoline recovers every one.
- Never lose a handle the ABI keeps open. Every close frees its handle, whatever it
  returns, so `Close` forgets the handle once the ABI's close has returned. The one
  exception is a subscription's close from inside a handler of its own poll, which the
  ABI refuses with `ErrPolling`, leaving the subscription open (`spec/c-abi.md`,
  "Calls during a claim or a poll"). `Close` then returns the `*Error` and keeps the
  handle, so the subscription can be polled and closed after the poll.
- Never edit a generated file by hand: `just lint` regenerates the module and fails
  when the committed files differ. `example/main.go` is the one file written by hand,
  and it is this README's program.

## Example

The ten-minute path's program (the README's `cavalry run`, the command that builds
the library from the same checkout, and `go run`), as `conformance/go/quickstart`
runs it with its checks. The program is `clients/go/example/main.go`;
`conformance/go/readme` checks that this page shows that file and that it prints the
line below:

```sh
cargo build --release -p cavalry --bin cavalry   # the driver
cargo build --release -p cavalry-ffi --lib       # libcavalry_ffi.a, which cgo links
target/release/cavalry run &                     # one driver per host
(cd clients/go && go run ./example)              # builds the package with cgo, then runs it
kill -TERM %1
```

```go
package main

import (
	"encoding/binary"
	"fmt"

	cavalry "github.com/appshapes/cavalry/clients/go"
)

func main() {
	client, err := cavalry.NewClient("") // the driver at CAVALRY_DIR, or its default
	if err != nil {
		panic(err)
	}
	defer client.Close()
	subscription, err := client.AddSubscription("cavalry:ipc", 1001)
	if err != nil {
		panic(err)
	}
	publication, err := client.AddPublication("cavalry:ipc", 1001)
	if err != nil {
		panic(err)
	}
	received := make([]uint32, 0, 1000)
	onFragment := func(body []byte, header *cavalry.Header) { // body: a slice into the log, for the call
		received = append(received, binary.LittleEndian.Uint32(body))
	}
	for sent := uint32(0); sent < 1000; {
		if publication.TryClaim(32) < 0 { // the position after the frame, or a code below 0
			subscription.Poll(onFragment, 10) // back-pressured or not yet connected: drain, retry
			continue
		}
		binary.LittleEndian.PutUint32(publication.Claim, sent) // write into the log
		publication.Commit()
		sent++
	}
	for len(received) < 1000 {
		subscription.Poll(onFragment, 10)
	}
	for i, value := range received {
		if value != uint32(i) {
			panic("every message once, in order")
		}
	}
	fmt.Printf("%d messages over cavalry:ipc, position %d\n", len(received), publication.Position())
}
```

You will see `1000 messages over cavalry:ipc, position 64000`.

## The runtime's side of the boundary

What cgo does per message, and what `conformance/go/quickstart`'s report measures of
it. The figures below are one run's, on the laptop (Go 1.27.0, macOS 26.6 on arm64,
unpinned); every CI run prints its runners'.

### The call

Each per-message function is one cgo call into the ABI, with its arguments as they
are: the handle as the C pointer the ABI returned, the claim struct as the address of
the one `cavalry_buffer_claim_t` the publication keeps, the offered bytes as the
address of the slice's first element. A cgo call switches the goroutine to the
system stack and back, which Go's runtime does in 14 ns on this laptop for a call
that returns at once (`cavalry_abi_version`, 5,000,000 calls), and cgo's pointer
check, on by default, reads each pointer argument once. The ABI's reply
comes back as the Go integer of the C type. An add, a command or a close waits on the
driver for up to 10 s (`spec/c-abi.md`, "Threads"); the goroutine blocks in the call
and the scheduler runs the program's other goroutines meanwhile.

### Threads

A `Client` and the handles it makes are used from one goroutine at a time; the client
adds no lock, and the ABI runs one call on a handle at a time. A program with several
goroutines gives each its own `Subscription` and `Publication`; the `Client` may be
shared, since each of its calls is one ABI call that the ABI serialises. The table the
trampoline reads subscriptions from is written under a lock when a subscription is
added or closed, never on a message's path, and read with one atomic load per
fragment.

### The call-back, by construction

The ABI calls the C trampoline in `shim.c` with the body's address and length and the
frame's header. That function calls the exported Go function `cavalryGoFragment`, a
cgo call-back: Go's runtime switches from the system stack to the goroutine that
called `Poll`, which is what the call costs, and nothing is allocated for it. The Go
trampoline reads the subscription from the table by the index the ABI handed back as
`clientd`, makes the body's `[]byte` over the log's bytes with `unsafe.Slice`, no copy
and no allocation, points the subscription's one `Header` at the frame, and calls the
handler inside a function that recovers a panic ("The panic"). By construction, no
object per received message.

The `Header`'s `Values` reads `cavalry_header_values` once per fragment, on the first
call, into the one `cavalry_header_values_t` the subscription keeps, and returns the
`Frame` by value, a struct of nine integers that lives on the caller's stack. Outside
the handler call `Values` returns `false`, since the header is valid for the call
alone. The body's slice is valid for the call alone too: it points into the log,
which the publisher writes over once the subscription's position has moved past it.
Go has no view that can be detached, so a slice kept past the call reads whatever the
log holds later; copy what must outlive the handler.

### The view over a claimed slice, by construction

`publication.Claim` is a `[]byte` over the bytes the ABI reserved, made with
`unsafe.Slice` from the pointer the ABI filled into the one `cavalry_buffer_claim_t`
the publication keeps, no allocation; `Commit`, `Abort` and `Close` set it to `nil`.
A slice kept past the commit points into the log, as a body does.

### The count, measured

The runner's allocation round sends 10,000 messages whose handler only counts and whose
publisher copies one constant, with `runtime.ReadMemStats` read before and after the
round with nothing between, which counts every allocation Go's heap made. On the
laptop: 0 allocations and 0 bytes per message over 9,999 messages, so a message
allocates nothing on either side of the boundary; a `runtime.GC` before the round
and the reads are the round's own cost.

### The latency, measured

The runner's latency round sends 100,000 messages over `cavalry:ipc`, one at a time on
one goroutine. The publisher writes the monotonic clock, `time.Since` a start, into
the claim before its commit, and the handler reads the clock first. On the laptop,
unpinned: p50 83 ns, p99 125 ns, p99.99 3,583 ns (10 samples above it, nearest
rank), max 5,500 ns. That is two cgo calls and one call-back per message
over the ABI's own one-way latency, which `just bench`'s `one_way_32_ffi` row
measures.

### The panic

A panic in the handler must not unwind through the ABI's frames: Go's runtime would
end the process at the first C frame it met. The Go trampoline calls the handler
inside a function that recovers, keeps the first panic's value, counts every one, and
goes on calling the handler for every later fragment of the poll, so that no fragment
is consumed unseen; `Poll` returns the count and a `*HandlerError` once the ABI has
returned, with `Fragments`, `Raised`, the first value as `Cause`, and a message that
says what to do. The value is kept as it was panicked, an `error`, a string or
anything else, and stays the first when an `error` follows it in the same poll.
`runtime.Goexit` in a handler is not a panic and is not recovered; a program that
calls it from a handler ends the goroutine that called `Poll` inside the ABI, which
the ABI's page does not allow.

## What to read next

- `spec/c-abi.md`: the page the module wraps, function for function.
- `clients/README.md`: how a client is generated and what every client passes.
- `clients/python/README.md`: the first client, whose shape this one copies.
- `conformance/go/`: the runner and its report.
- `docs/decisions/0046-build-the-c-abi-and-the-clients-fourth.md`: the slice this
  client is the fourth feature of.
