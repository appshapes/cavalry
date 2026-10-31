# Set up a soak host

A how-to page: one task, turning a stock Linux machine into the host that runs a
driver recording a steady stream for days, with `doctor` and the counters file sampled
every minute, so that what CI sees for seconds is seen for a week: bounded queues,
counter reuse after the reuse timeout, the catalog's slots, the segment files under
retention, and whatever else drifts. The soak is not a gate and blocks no merge. Why the
host is `kip` and not the gate's second host is
[ADR 0034](../decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md); what the
retention commands the soak runs do is
[Record a stream](record-a-stream.md), steps 3 to 6.

## Prerequisites

- A Linux machine with a drive that has room for days of recordings. At the defaults,
  10,000 messages a second of 32 bytes, the stream is about 2.3 GB an hour before
  retention and the soak keeps a day of it, so 100 GB is enough; `kip` has 3.6 TB.
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
   writes `cavalry doctor`'s report and a copy of the counters file under `samples/`,
   and every hour it stops the recording, starts a new one, purges the segments of the
   one it stopped before the middle of its hour and deletes every recording older than
   a day, so that retention runs on a host whose disk would otherwise fill:

   ```sh
   # ignore: run on the soak host, not in this repository
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
   `ls /srv/cavalry-soak/samples` lists `doctor-<time>.txt` and `counters-<time>`.

5. Read a sample. `cat /srv/cavalry-soak/samples/doctor-<time>.txt` is the report
   `cavalry doctor --dir /dev/shm/cavalry-soak --log-dir /srv/cavalry-soak/log` printed
   at that minute, ending in `exit <code>`: the `recording` line with the recording's
   position and synced position, the counters file's fill, and every finding; a soak
   that is doing what it should prints one `WARN`, `host-governor`, since the stock
   governor is what a soak host keeps on purpose, and exits 0. The `counters-<time>`
   file is the counters file's bytes at that minute, for a reader of the counters
   layout (`docs/reference/counters-file.md`) when a counter's course over the week is
   wanted, which the week's close-out reads.

6. Stop it, when the week is up or when something has to change:

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
- `df -h /srv/cavalry-soak` holds at about 30 GB at the defaults from the second day
  on. A day of recordings would be 55 GB; the purge of each stopped recording's first
  half leaves about 1.15 GB of each of the 24 kept, plus the live hour's. A reading
  near 55 GB says the purges were refused, and the log says with which error.
- The program's lines keep `behind 0` and `back-pressured 0` at the default rate; the
  samples' doctor reports keep exiting 0 with `host-governor` as their one finding.

## When it does not work

- The service exits at once with `no binaries at target/release`: step 2 was not run
  from the clone the unit's `WorkingDirectory` names.
- `soak.sh: the driver did not come up`: `/dev/shm/cavalry-soak` is held by a driver
  from an earlier run, or `doctor` fails on it; run `target/release/cavalry doctor --dir
  /dev/shm/cavalry-soak` by hand and read the finding.
- `record 1001 failed: ... error 20`: the soak program is not publishing yet; the script
  retries at the next rotation. If it keeps failing, the program's own line in the log
  says why it stopped.
- The program exits with `message <n> arrived when <m> was next`: a message out of
  order or twice, which never happens (invariant 1); the log, the samples and the
  recording at that minute are the evidence, and the next step is a card with them.
- A `warn` in a sample's doctor report, such as `log-directory-space` or
  `recording-over-half`, names the retention command to send and the docs anchor; the
  soak's rotation should have sent it already, and the log says whether the purge or
  the delete was refused and with which error.
