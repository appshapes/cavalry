# cavalry-control

The admin API, discovery, OpenTelemetry export and configuration. Control plane: `async`
is allowed here.

## What this does

- Exports OpenTelemetry metrics and traces from the counters file. Counters are the
  primary instrument; this crate turns them into something a dashboard can read, at zero
  cost to the data plane.
- Serves the admin API and discovery.
- Loads configuration. Every key lives in `config.toml` with a non-empty
  `why_not_default`, a `doctor` check that fires when it is set wrong, and a line in the
  generated reference.
- May use `async` and an executor: nothing a message passes through lives here.

In Aeron, this is AeronStat and hand-written tooling: the open project has no admin API
and no OpenTelemetry export ([glossary](../../docs/glossary.md)).

## What it must never do

- Sit on the path a message takes. The data plane never waits on this crate, and no
  data-plane crate may depend on it; `just lint` fails that dependency.
- Add a lock, a syscall or an allocation to the data plane by reaching into shared
  state. It reads the counters file; it does not write the hot path's structures.
- Replace counters with logs as the primary instrument (invariant 4). Logs and traces
  are derived from counters and are zero-cost when disabled.
- Add a configuration key without a written reason why the default cannot be right
  (invariant 5). Tune by detection, not by documentation.

## Example

Not yet. This crate has no public item to show, and a crate README's example is also
its doctest, so an example written now would invent an API.
[ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) records why the example
rule is deferred for scaffold crates. The example arrives in the same commit as the
first public item.

## What to read next

Not yet: the pages this crate will point at, its section of `spec/` and its reference
page, do not exist. Until they do, read [`CLAUDE.md`](../../CLAUDE.md) for the
invariants and the gates, and
[ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) for how the workspace is
laid out.
