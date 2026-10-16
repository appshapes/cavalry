# Run a driver and read doctor

A how-to page: one task, start a driver on this host, see that it is running, and stop
it. The driver is the one daemon per host that clients attach to through shared memory;
what it keeps in its directory is [`spec/client-protocol.md`](../reference/client-protocol.md).
In Aeron, this is the media driver ([glossary](../glossary.md)).

## Prerequisites

- A checkout of this repository and the Rust toolchain; `cargo build -p cavalry`
  passes. There is no release yet, so the binary is run from the checkout.
- On Linux, `/dev/shm` mounted, which it is on every common distribution. The driver's
  directory goes there; elsewhere it goes under the temporary directory, where
  write-back can stall a publisher, and `doctor` says so.
- No other driver on this host for the same user, or a directory of your own for this
  one: `--dir <path>`. A second driver on the same directory is refused with
  `driver-active`, which names the directory and the pid.

## Steps

1. Start the driver. It needs no arguments and no configuration:

   ```sh
   cargo run --release -p cavalry -- run
   ```

   You will see one line on stderr: `cavalry run: driver started on
   /dev/shm/cavalry-<user> (pid <pid>), recording into
   ~/.local/share/cavalry/log, conductor idle strategy backoff; Ctrl-C or
   SIGTERM stops it`. The directory now holds `counters`, `command-ring` and
   `broadcast-buffer`, and a `publications/` directory appears with the first
   publication.

   To run it elsewhere, or a second one beside the first, give the directory:
   `cargo run --release -p cavalry -- run --dir /dev/shm/cavalry-two`, or set
   `CAVALRY_DIR`; the flag wins. On a host with isolated CPUs, run the driver on one of
   them with `CAVALRY_CONDUCTOR_IDLE_STRATEGY=busy-spin`. Those are the two
   configuration keys this version has; [the configuration reference](../reference/config.md)
   says what each does and why its default is not always right.

2. In another terminal, read `doctor`:

   ```sh
   just doctor
   ```

   You will see a report whose first line is `cavalry doctor: <directory>, checks run:
   <n>` and whose `driver-heartbeat` line is a `note` saying the driver is running, with
   its pid and uptime. The last line counts failures and warnings; `just doctor` exits 0
   when nothing failed. On Linux the host checks follow: a governor that is not
   `performance` and transparent huge pages set to `always` are warnings, and isolated
   CPUs are a note recommending the busy-spin idle strategy there. A driver on another
   directory is read with `cargo run --release -p cavalry -- doctor --dir <path>`, or
   with `CAVALRY_DIR` set as it was for `run`.

3. Stop the driver with Ctrl-C in its terminal, or `kill -TERM <pid>`. You will see
   `cavalry run: stopping` and then `cavalry run: closed; <directory> stays for doctor,
   and the next driver remakes it`. The directory stays, with −1 in the command ring's
   heartbeat, so that `doctor` can still read the counters of the driver that has gone.

## How to verify it worked

`just doctor` after the stop exits 1 and its first line after the header is `FAIL
driver-heartbeat`, saying the driver closed and what to do: start one with `cavalry
run`. Starting one again removes the old directory and makes it anew; `doctor` then
reports it running with a new pid.

## When it does not work

- **`driver refused (driver-active)`.** Another driver is alive on the directory, and
  the message names its pid. Stop that one, or leave it: one driver per host is the
  shape.
- **`FAIL driver-heartbeat ... stopped without closing`.** A driver died without
  storing −1: its process was killed with SIGKILL or crashed. Its counters are still
  readable; the next `cavalry run` remakes the directory.
- **A warning per counter.** Every static counter with a non-empty `doctor` text in
  `counters.toml` is reported when it is above 0, with that text, which says what the
  operator does next; [the counters reference](../reference/counters.md) lists them.
- **`WARN driver-directory-backing`.** The directory is on a filesystem that writes back
  to a device, so a publisher's writes to a log buffer can stall on write-back. Set
  `CAVALRY_DIR` to a directory on a tmpfs, `/dev/shm` on most Linux hosts, for the
  driver and every client, or mount one there. On macOS this line is a `note`: there is
  no tmpfs by default, and the default directory is the best there is.
- **`WARN conductor-idle-strategy`.** The conductor busy-spins and no CPU is isolated,
  so it takes a whole core from the clients. Set `CAVALRY_CONDUCTOR_IDLE_STRATEGY` to
  `backoff`, or isolate a core and pin the driver there. The same check notes isolated
  CPUs a backoff conductor is not using.
- **`configuration refused (invalid)`, exit 2.** A key was set to a value it does not
  take, `CAVALRY_CONDUCTOR_IDLE_STRATEGY=spin` for example; the message names the
  values the key takes and the default. Nothing started, and no directory was touched.
