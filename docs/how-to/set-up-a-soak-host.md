# Set up a soak host

A how-to page: one task, turning a stock Linux machine into the host that runs a
driver recording a steady stream for days, with `doctor`, the counters file and the
memory, threads and open descriptors of the soak's two processes sampled every minute,
so that what CI sees for seconds is seen for a week: bounded queues, counter reuse
after the reuse timeout, the catalog's slots, the segment files under retention, a
process that grows, and whatever else drifts. The soak is not a gate and blocks no merge. Why the
host is `kip` and not the gate's second host is
[ADR 0034](../decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md); what the
retention commands the soak runs do is
[Record a stream](record-a-stream.md), steps 3 to 6.

## Prerequisites

- A Linux machine with a drive that has room for days of recordings and samples. At
  the defaults, 10,000 messages a second of 32 bytes, the stream is about 2.3 GB an
  hour before retention; the soak keeps a day of it, purged to about 30 GB, and the
  samples add about 9 GB a day, since each minute's copy of the counters file is
  6 MiB and nothing removes them. A week is about 93 GB, so 120 GB is enough for a
  week, and each further week needs 63 GB more; `kip` has 3.6 TB.
  Nothing on the host is tuned: no isolated CPUs, no fixed clock, a stock kernel line,
  a stock governor and swap. A soak host is any machine, and a tuned one would hide
  what the stock one shows. `kip` arrives tuned, as the gate's second host until
  [ADR 0034](../decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md), so
  un-tune it first, by hand, as the ADR's Consequences say: delete the files
  `bench/host/setup-ubuntu.sh` names, `systemctl unmask` the timers it masked, run
  `update-grub`, reboot, and remove its runner. The script's header and each of its
  steps carry an `Undo:` line naming the file or the timer. The `host-governor`
  finding that step 5 expects appears only once the governor is stock again.
- Ubuntu Server 24.04 LTS or later, with one administrative user, key-only SSH and
  passwordless `sudo` for that user, as the bench hosts have
  ([Set up a bench host](set-up-a-bench-host.md)), so that a session can maintain the
  host.
- The Rust toolchain for that user, `rustup` with the stable channel, and a clone of
  this repository under the user's home.
- The soak's stream, 1001 at the default, on the soak's driver is the soak's alone.
  The program counts what it receives across the subscription, not per publisher, so
  a second publisher on that stream, a replay onto it among them, ends the run as a
  message out of order. Anything else you run on the host takes another stream id or
  another driver directory.

## Steps

1. Give the log directory a volume of its own, so that a filling disk fills nothing
   else. On `kip`, the installer left the volume group with the drive's space
   unallocated:

   ```sh
   # ignore: run on the soak host, not in this repository
   sudo lvcreate -L 1T -n soak ubuntu-vg
   sudo mkfs.ext4 -L cavalry-soak /dev/ubuntu-vg/soak
   sudo mkdir -p /srv/cavalry-soak
   echo 'LABEL=cavalry-soak /srv/cavalry-soak ext4 defaults,noatime 0 2' | sudo tee -a /etc/fstab
   sudo mount /srv/cavalry-soak
   sudo chown "$USER" /srv/cavalry-soak
   ```

   You will see the volume under `df -h /srv/cavalry-soak` with its size and nothing
   used. The soak's log directory, samples and state live under it:
   `/srv/cavalry-soak/log`, `/srv/cavalry-soak/samples` and
   `/srv/cavalry-soak/recordings.txt`.

2. Build the binaries the soak runs, the `cavalry` binary and the soak program, from
   the clone:

   ```sh
   # ignore: run on the soak host, not in this repository
   cd ~/cavalry && git pull --no-edit
   cargo build --release -p cavalry --bin cavalry --example soak
   ```

   The build ends with `Finished`, and `target/release/cavalry` and
   `target/release/examples/soak` exist. The soak program is
   `crates/cavalry/examples/soak.rs`: one process that publishes the stream at a fixed
   rate and reads it back through a subscription, each message once and in order, and
   prints one line a minute with what it sent and received.

3. Install the service. It runs `scripts/soak.sh` as the administrative user from the
   clone, which starts the driver on `/dev/shm/cavalry-soak` with the log directory on
   the volume, the soak program, and a recording of the stream; then every minute it
   writes `cavalry doctor`'s report and a copy of the counters file under `samples/`
   and adds one line for the driver and one for the program to the run's file of
   process samples, `samples/processes-<start>.txt`,
   and every hour it stops the recording, starts a new one, purges the segments of the
   one it stopped before the middle of its hour and deletes every recording older than
   a day, so that retention runs on a host whose disk would otherwise fill. First let
   the user's processes outlive the user's logins: the service runs as the
   administrative user, a service's processes are not a session, and when that user's
   last SSH session ends logind removes every POSIX shared-memory object the user
   owns, the driver's files under `/dev/shm` among them (`RemoveIPC=yes`, its default),
   unless the user lingers:

   ```sh
   # ignore: run on the soak host, not in this repository
   sudo loginctl enable-linger "$USER"
   loginctl show-user "$USER" -p Linger    # Linger=yes
   sudo tee /etc/systemd/system/cavalry-soak.service >/dev/null <<UNIT
   [Unit]
   Description=Cavalry soak: a driver recording a steady stream for days
   After=network.target srv-cavalry\x2dsoak.mount
   Requires=srv-cavalry\x2dsoak.mount

   [Service]
   User=$USER
   WorkingDirectory=$HOME/cavalry
   Environment=SOAK_DIR=/srv/cavalry-soak CAVALRY_DIR=/dev/shm/cavalry-soak
   ExecStart=/usr/bin/env bash scripts/soak.sh
   KillSignal=SIGTERM
   KillMode=mixed
   TimeoutStopSec=60
   StandardOutput=append:/srv/cavalry-soak/soak.log
   StandardError=append:/srv/cavalry-soak/soak.log
   Restart=no

   [Install]
   WantedBy=multi-user.target
   UNIT
   sudo systemctl daemon-reload
   sudo systemctl enable --now cavalry-soak.service
   ```

   `Restart=no` is deliberate: a soak that stops on its own has found something, and
   the log says what; a restart would hide it. The script exits 1 when the program or
   the driver ended the run, so `systemctl status cavalry-soak.service` then shows
   `failed`, and `inactive` only after a stop you asked for. `KillMode=mixed` is
   deliberate too: systemd's default, `control-group`, sends the stop's SIGTERM to
   every process in the unit at once, so the program and the driver would die before
   the script could stop them in order, and the log would read as a finding;
   `mixed` sends it to the script alone, which stops the program, the recording and
   the driver in turn, and systemd sends SIGKILL to whatever is left only after the
   script has exited or `TimeoutStopSec` has passed. The script's settings, the stream, the
   rate, the body, the sample and rotation intervals and how many rotations are kept,
   are environment variables with defaults in its header; set them in the unit's
   `Environment=` line to change them.

4. Watch it start:

   ```sh
   # ignore: run on the soak host, not in this repository
   tail -f /srv/cavalry-soak/soak.log
   ```

   You will see `soak.sh: <time> starting: driver on /dev/shm/cavalry-soak, ...`, the
   program's `soak: <time> client <id> on /dev/shm/cavalry-soak publishing stream 1001
   ... at 10000 messages/s`, then `soak.sh: <time> recording 0 started`, and from then
   on one line a minute from the program, `soak: <time> sent <n> (+600000) received <n>
   (+600000) behind 0 back-pressured 0 not-connected 0 position <p>`, where the two
   increments are the minute's messages and `behind` is how many it has sent that the
   subscription has not read yet. The first sample is written at once:
   `ls /srv/cavalry-soak/samples` lists `doctor-<time>.txt`, `counters-<time>` and
   `processes-<start>.txt`.

5. Read a sample. `cat /srv/cavalry-soak/samples/doctor-<time>.txt` is the report
   `cavalry doctor --dir /dev/shm/cavalry-soak --log-dir /srv/cavalry-soak/log` printed
   at that minute, ending in `exit <code>`: the `recording` line with the recording's
   position and synced position, the counters file's fill, and every finding; a soak
   that is doing what it should prints one `WARN`, `host-governor`, since the stock
   governor is what a soak host keeps on purpose, and exits 0. The `counters-<time>`
   file is the counters file's bytes at that minute, for a reader of the counters
   layout (`docs/reference/counters-file.md`) when a counter's course over the week is
   wanted, which the week's close-out reads. `processes-<start>.txt` is one file for
   the whole run, named by the time the run started, so a service that is stopped
   and started again leaves one file for each run. Each sample adds two lines to it,
   one for the driver and one for the program, with the sample's `<time>`:
   `<time> driver pid=<pid> rss_kib=<n> anon_kib=<n> file_kib=<n> shmem_kib=<n>
   vsz_kib=<n> threads=<n> fds=<n>`. The values are the kernel's, read from
   `/proc/<pid>/status` and `/proc/<pid>/fd`:

   | Field | What it is |
   | --- | --- |
   | `rss_kib` | The process's resident memory, in KiB: the pages of it that are in RAM. It is the sum of the next three. |
   | `anon_kib` | The part that belongs to no file: the heap and the threads' stacks. |
   | `file_kib` | The part mapped from files on a disk: the binary and its libraries among them. |
   | `shmem_kib` | The part mapped from files on a tmpfs. The driver's directory, `/dev/shm/cavalry-soak`, is on one, so this is the driver's rings, its counters file and the stream's log buffer. |
   | `vsz_kib` | The virtual size, in KiB: every mapping, in RAM or not. |
   | `threads` | The process's threads. |
   | `fds` | Its open file descriptors. |

   A value the script could not read is `-`, never 0, and a process that ended before
   the script looked has `<time> <name> pid=<pid> gone` for its line. On a machine
   with no `/proc`, a macOS laptop, the script reads `rss_kib`, `vsz_kib` and
   `threads` from `ps` and `fds` from `lsof`, and the three parts read `-`.

6. Read a course, a value's readings in order across the run, from the first sample
   and the last of each process:

   ```sh
   # ignore: run on the soak host, not in this repository
   run=$(ls /srv/cavalry-soak/samples/processes-*.txt | tail -1)
   grep ' driver ' "$run" | sed -n '1p;$p'
   grep ' soak ' "$run" | sed -n '1p;$p'
   du -sk /dev/shm/cavalry-soak
   ```

   `run` is the newest run's file. You will see two lines for the driver, two for
   the program, and the size of the driver's directory in KiB. Read them in this order, since most of a driver's
   resident memory is not growth:

   - `shmem_kib` is mappings, not growth. The driver maps every file of its
     directory and brings every page of a mapping into RAM at the moment it maps it
     ([ADR 0027](../decisions/0027-pre-touch-shared-mappings-at-map-time.md)), so the
     driver's `shmem_kib` is the directory's size from the first sample on: on `kip`
     both read 204816, which is the stream's log buffer, 196612, the counters file,
     6148, and the two rings. A client does the same with what it maps, and the
     program maps the log buffer twice, as the publication and as the subscription's
     image, and the counters file and the command ring twice, so its `shmem_kib` is
     about twice the driver's. A `shmem_kib` that is the same on the first line and
     the last is these mappings and nothing else. One that rose has a file behind
     it: `ls -l /dev/shm/cavalry-soak/publications` lists one log buffer for each
     publication, and a soak adds none after its start. A binary that is itself on a
     tmpfs, as one built under `/tmp` is on a host whose `/tmp` is one, adds its own
     pages to `shmem_kib` and takes them from `file_kib`.
   - `anon_kib`, `threads` and `fds` are where growth shows. A driver allocates when
     it starts and when a client adds or removes something, and never per message
     (`CLAUDE.md`, invariant 2), so over a week of one stream these hold level. On
     `kip` the driver read `anon_kib=600 threads=7 fds=8` on each of an hour's first
     sixty samples and `anon_kib=616` on the sixty-first, the one after the hour's
     rotation; and a driver of an earlier build, ten days into its run, read
     `anon_kib=784 threads=5 fds=8` at every sample of an hour. A value
     that is higher on the last line than on the first is read across the whole file
     next, `awk '$2 == "driver" { print $1, $5, $9, $10 }' "$run"`: a step that
     happened once has a cause
     in `soak.log` at that minute, as the rotation is of those 16 KiB, and a value
     that rises hour after hour is a finding, with a card as its next step.
   - `rss_kib` and `vsz_kib` follow from the others. Use `rss_kib` alone only where
     the parts read `-`. There the mappings are in the first line already, so what
     the last line adds to the first is growth, and the line cannot say of which
     part: on a macOS laptop an hour's run added 224 KiB to the driver's 204416, one
     16 KiB page about every six minutes, where the same build on `kip` added the
     rotation's 16 KiB and nothing else.

7. Stop it, when the week is up or when something has to change:

   ```sh
   # ignore: run on the soak host, not in this repository
   sudo systemctl stop cavalry-soak.service
   ```

   The log ends with `soak.sh: <time> stopping`, the program's `done: <n> sent, <n>
   received once and in order`, `recording <id> stopped`, `driver exited 0` and
   `stopped; the recordings are under ... and the samples under ...`. The recordings
   and the samples stay on the volume until you remove them.

## How to verify it worked

- After an hour, `grep -E 'stopped|started|purged|deleted' /srv/cavalry-soak/soak.log`
  shows `recording 0 stopped`, `recording 1 started` and `recording 0 purged before
  <base>`, and
  `cat /srv/cavalry-soak/recordings.txt` lists each recording with its start and
  state; after a day the oldest reads `deleted` and its files are gone from
  `/srv/cavalry-soak/log`.
- `du -sh /srv/cavalry-soak/log /srv/cavalry-soak/samples` shows the volume's two
  parts. The recordings under `log` hold at about 30 GB at the defaults from the
  second day on: a day of recordings would be 55 GB, and the purge of each stopped
  recording's first half leaves about 1.15 GB of each of the 24 kept, plus the live
  hour's. The samples grow by about 9 GB a day and are never removed: each minute's
  copy of the counters file is 6 MiB, the whole file at the driver's capacity of 16,384
  counters (`docs/reference/counters-file.md`; 3 MiB and 4.5 GB a day before client
  protocol version 4), 1,440 copies a day, and the doctor reports beside them are a few
  KB each. So `df -h /srv/cavalry-soak` reads about 48 GB on the second day and about
  93 GB at the end of a week. A `log` near 55 GB,
  which is a `df` about 25 GB above that line, says the purges were refused, and
  `soak.log` says with which error.
- The program's lines keep `behind 0` and `back-pressured 0` at the default rate; the
  samples' doctor reports keep exiting 0 with `host-governor` as their one finding.
- After an hour, `grep -c ' driver ' /srv/cavalry-soak/samples/processes-*.txt` and
  the same for `' soak '` each print 60 or 61 for the run's file: one line at the
  start and one a minute, each a little more than a minute after the last. The first
  and the last line of each process agree on `shmem_kib`, `threads` and `fds`. Their
  `anon_kib` can differ by the rotation's step, 16 KiB for the driver on `kip`, and
  step 6 says how to read a larger difference.

## When it does not work

- The service exits at once with `no binaries at target/release`: step 2 was not run
  from the clone the unit's `WorkingDirectory` names.
- The samples turn to `FAIL driver-directory ... no driver directory at
  /dev/shm/cavalry-soak` and `soak.sh: no counters file to copy` while the program's
  lines go on: the driver's directory was removed under a running driver, which keeps
  working on its mappings but which no new client can reach, so the next rotation's
  `cavalry record` is refused with `no driver at /dev/shm/cavalry-soak`. On the first
  run on `kip` this happened two minutes in, when the operator's SSH session ended and
  logind removed the user's shared memory (`RemoveIPC=yes`), because step 3's
  `loginctl enable-linger` had not been run. Enable lingering, then stop and start the
  service: the stop is clean, the recording's stop is refused for want of the
  directory, and the restart's recovery rebuilds that recording's stop from its files.
- `soak.sh: the driver did not come up`: `/dev/shm/cavalry-soak` is held by a driver
  from an earlier run, or `doctor` fails on it; run `target/release/cavalry doctor --dir
  /dev/shm/cavalry-soak` by hand and read the finding.
- `record 1001 failed: ... error 20`: the soak program is not publishing yet; the script
  retries at the next rotation. If it keeps failing, the program's own line in the log
  says why it stopped.
- The program exits with `message <n> arrived when <m> was next`: a message out of
  order or twice, which never happens (invariant 1); the log, the samples and the
  recording at that minute are the evidence, and the next step is a card with them.
  `a <n>-byte message arrived when <m> was next ... not this program's` is a message
  from another publisher on the stream, which the prerequisites rule out; find that
  publisher before reading anything else into the run.
- A `warn` in a sample's doctor report, such as `log-directory-space` or
  `recording-over-half`, names the retention command to send and the docs anchor; the
  soak's rotation should have sent it already, and the log says whether the purge or
  the delete was refused and with which error.
