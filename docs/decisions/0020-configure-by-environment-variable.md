# 0020. Configure by environment variable, and register every key in config.toml

- **Status:** Accepted
- **Date:** 2026-10-12
- **Invariants affected:** 2, 4, 5, 6
- **Supersedes:** none

## Context

Invariant 5 makes zero configuration the primary supported configuration, and
`CLAUDE.md` makes a configuration key expensive on purpose: a written reason why the
default cannot be right, a `doctor` check that fires when it is set wrong, a line in a
generated reference, and `just lint-config` to refuse a key without them. Until card 86,
no key existed, `config.toml` did not exist, and `lint-config` printed `not built yet`
(ADR 0003; ADR 0007, which gives this gate to the driver feature). The `cavalry` binary
took no arguments, and the driver's directory and idle strategy were fixed at
`spec/client-protocol.md`'s default and backoff.

Two defaults cannot always be right. The directory: one driver per host and user is
right on a laptop and on a cloud VM, and wrong for two drivers on one host, for a
container whose `/dev/shm` is 64 MiB or absent, and for a host whose memory-backed
filesystem is mounted elsewhere; and every client must find the driver by the same
directory, so the key reaches the client library in feature 5 as well. The conductor's
idle strategy: backoff is right where a spinning conductor would take a core from the
clients, and busy-spin is right on an isolated core, where backoff's sleep of up to 1 ms
delays the publication limit a publisher waits on (invariant 2); `doctor` already
recommends busy-spin on isolated cores (`CLAUDE.md`, "Data plane"), and nothing could
act on the recommendation. The keys were presented to the owner before they were added,
as `CLAUDE.md`, "Ask before" requires, together with the question this ADR answers:
where a value that is not typed on a command line comes from, secrets included.

Aeron reads its keys as Java system properties in the Java driver (`aeron.dir`,
`aeron.conductor.idle.strategy`) and as environment variables in the C driver
(`AERON_DIR`, `AERON_CONDUCTOR_IDLE_STRATEGY`), the property name upper-cased with dots
as underscores; `aeronmd` also takes `-D key=value` and properties files. Read from
`aeron-driver/.../Configuration.java` and `aeron-driver/src/main/c/aeron_driver_context.c`
on 2026-10-12.

## Decision

`config.toml` at the repository root is the registry of every configuration key: name,
type, default, flag, glossary term, what it sets, `why_not_default` and the name of its
`doctor` check. `scripts/gen-config.py` checks it and generates `docs/reference/config.md`
and the key table in `cavalry-control`, and `just lint-config`, part of `just lint`,
fails when either is stale, when a field is missing or empty, when the named `doctor`
check is not a finding in `doctor.rs` with a test naming it, when a key's flag is not
parsed by the binary, or when the module that reads the environment does not use the
key. A key is read from, first set wins: its flag on the `cavalry` binary, when it has
one; its environment variable, which every key has, `CAVALRY_` and the name upper-cased
with dots as underscores; its default. An empty value is an unset one. A value a key
does not take stops the command before anything starts, exit 2, with a message naming
the key, the value, the values it takes and the key's section of the reference page. A
flag exists only for a key an operator types interactively; a key set once in a unit
file or a container's environment has none. There is no operator-facing configuration
file in this version. A secret, when the first one exists, is marked as such in the
registry, has no flag and no default, is read from its environment variable or from the
file named by the variable with the suffix `_FILE`, is never printed, and `doctor`
reports it as set or unset; `lint-config` refuses a flag for it. Nothing is built for
secrets until the first one exists.

Two keys open the registry. `dir`, Aeron's `aeron.dir` without the prefix:
`CAVALRY_DIR`, `--dir` on `run` and `doctor`; its check `driver-directory-backing` warns
when the directory is on a filesystem that is not in memory, read from
`/proc/self/mountinfo` on Linux, and notes that it cannot tell elsewhere.
`conductor.idle.strategy`, Aeron's name: `CAVALRY_CONDUCTOR_IDLE_STRATEGY`, no flag,
values `backoff`, `yield`, `busy-spin`, the words of `CLAUDE.md` and the glossary; the
driver records its choice once, at start, in the static gauge `conductor.idle_strategy`
(0, 1, 2 in that order), so that its check `conductor-idle-strategy` reads the running
driver's strategy from the counters file alone (invariant 4): a warning for busy-spin
on a host with no isolated CPU, a note for backoff or yield on a host with one, a
warning for a value `doctor` does not know. Configuration is control plane: it is read
once, before the driver starts, and nothing on a duty cycle or the message path reads it.

## Alternatives considered

- **Zero keys for this slice, and `lint-config` over an empty registry.** Lost: a lint
  that checks nothing proves nothing, the two defaults above are wrong on hosts the
  first release will run on, and feature 5's clients need the directory key either way.
- **Flags for every key.** Lost: a key set in a unit file or a container's environment
  is typed once and read by every process, the driver and its clients, and only the
  environment reaches an embedded driver or a client library without an API change. A
  flag stays for the key an operator types at a terminal, which today is `--dir`.
- **An operator-facing configuration file, TOML or properties, as `aeronmd` takes.**
  Lost for now: zero configuration is the primary path, two keys do not justify a file
  format, a file is one more place a value can hide from `doctor`, and a secret in a
  file on disk needs the permissions story a `_FILE` reference leaves to the operator's
  secret store. Revisit when a deployment has enough keys to want one.
- **Secrets on the command line, or in the registry's defaults.** Lost: a command line
  is readable by every user of the host through `ps`, and a default secret is not one.
  Environment variables and `_FILE` references are what `systemd`'s `LoadCredential`
  and Docker's secrets hand a process.
- **Choose busy-spin by detection: spin when the driver's affinity mask lies within the
  isolated CPUs.** Lost for now: it burns a core the operator did not ask for, a wrong
  detection is silent where a key with a `doctor` check is loud, and Aeron makes it a
  key. Revisit when the sender and receiver threads arrive with pinning, which is when
  the driver knows which cores it was given.
- **Tell `doctor` the running strategy through a file in the driver directory instead of
  a counter.** Lost: the directory's contents are the client protocol's page, so a file
  is a spec change with an ADR for one integer, and invariant 4 says `doctor` explains
  from counters alone.
- **Aeron C's value names, `spinning`, `yielding`, `backoff`.** Lost: `CLAUDE.md` and
  the glossary say busy-spin, yield and backoff, the Rust enum is named so, and one
  spelling with no aliases is what a reader can grep for.

## Evidence

None measured: a control-plane decision. What informed it: invariant 5 and the gate
table of `CLAUDE.md`; card 86's "Done when", which names `config.toml` and
`lint-config`; the two Aeron sources above; the owner's requirement, stated on
2026-10-12, that a secret is never passed on a command line and that environment
variables are the expected channel; and `doctor`'s `host-isolated-cpus` note, which
recommended a strategy nothing could set.

## Consequences

Easier: a key is one registry entry, one check and one line of parsing, and the lint
refuses a key that is missing any of them; the reference page cannot drift from the
registry; a client in feature 5 finds the driver through `CAVALRY_DIR` with no API of
its own; `doctor` can say which strategy a driver runs. Harder: every future key argues
its `why_not_default` and writes its check before it merges, by design; the binary has
a flag parser to keep small; a secret key, when it comes, needs the `_FILE` reader and
the `doctor` set-or-unset report this ADR promises. To maintain: `scripts/gen-config.py`,
the generated page and table, and the two checks.

Revisit: when the first secret key exists, which builds the `_FILE` reader and the
`doctor` report; when the sender and receiver threads arrive with pinning, which is
when idle strategies per thread and detection of isolated cores are decided; when a
deployment wants an operator-facing configuration file; when a key for a timeout or a
size of `spec/client-protocol.md` is justified, which ADR 0019's Revisit assigns to the
driver's reference, this page.

## References

- `config.toml`, `scripts/gen-config.py`, `docs/reference/config.md`
- `crates/cavalry-control/src/config/mod.rs`, `crates/cavalry-control/src/doctor.rs`
  (`driver-directory-backing`, `conductor-idle-strategy`),
  `crates/cavalry/src/main.rs`
- `counters.toml`, `conductor.idle_strategy`; `docs/how-to/run-the-driver.md`
- ADR 0003 (where the lints live), ADR 0007 (the slice; `lint-config` with the driver),
  ADR 0012 (counters as the instrument), ADR 0019 (the directory and the values the
  driver uses)
- Aeron, `aeron-driver/src/main/java/io/aeron/driver/Configuration.java`,
  `aeron-client/src/main/java/io/aeron/CommonContext.java`,
  `aeron-driver/src/main/c/aeron_driver_context.c`, read 2026-10-12
