# Set up a bench host for the latency gate

A how-to page: one task, turning a Linux machine into the host that runs the latency
gate, `just bench` and `just bench-compare`, as a self-hosted GitHub Actions runner. Why
the gate runs on a dedicated host and not on a shared runner is
[ADR 0016](../decisions/0016-run-the-latency-gate-on-a-dedicated-host.md); how to run
the harness and read its table is [Run the bench](run-the-bench.md).

## Prerequisites

- A machine that will do nothing else. The gate is a 5 percent rule on p99.99, and one
  other process on the measured cores moves p99.99 by more than that. An 8-core desktop
  part is enough; the first host, `bench`, is an AMD Ryzen 7 2700X with 60 GiB, and
  the second, `grip`, a Dell with an Intel Core i7-10700, eight cores under one L3,
  and 16 GB. `kip`, a Minisforum MS-01 with an Intel Core i9-13900H and 64 GB, whose
  six P-cores are the part that counts, ran the gate from 2026-10-29 to 2026-10-31 and
  is the soak host
  ([ADR 0034](../decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md)).
- Ubuntu Server, 24.04 LTS or later, installed with one administrative user and SSH.
  The scripts use systemd, GRUB and apt and have run on 26.04.1.
- Administrator rights on the GitHub organization, to register a runner and put it in
  a runner group, and on the repository, to add the job to the required checks.

## Steps

1. Choose the CPUs. On the host, run `lscpu -e` and read the `L3` column: CPUs that
   share an L3 cache are one core complex, and a line moved between two complexes costs
   about 100 ns more than one moved inside one. The harness needs four cores: its
   publisher, its subscriber and the driver's conductor in the shared-memory rows, and
   the driver's sender and receiver in place of the conductor in the UDP rows
   ([ADR 0044](../decisions/0044-gate-udp-on-the-two-bench-hosts.md)). Isolate one
   whole complex and leave the other to everything else. On the Ryzen 7 2700X, CPUs 0 to 7 are one complex and 8 to 15 the other, with
   each core's two SMT threads numbered 2n and 2n+1; the isolated set is `8-15` and
   the housekeeping set `0-7`, which are the scripts' defaults.

   On a hybrid Intel part, `/sys/devices/cpu_core/cpus` names the P-cores and
   `/sys/devices/cpu_atom/cpus` the E-cores. Isolate three P-cores with their SMT
   siblings and leave the other P-cores and every E-core to everything else; an E-core
   never carries a measured thread. On the i9-13900H the P-cores are CPUs 0 to 11 in
   sibling pairs 2n and 2n+1, the E-cores 12 to 19, with one L3 for all; the isolated
   set is `6-11`, the housekeeping set `0-5,12-19`, and `nosmt` leaves 6, 8 and 10
   online for `--pin 6,8,10`, three cores: `kip` ran the gate before the UDP rows.

   On the Intel Core i7-10700, eight cores under one L3 with each core's two SMT
   threads numbered n and n+8, there is no complex to choose: the isolated set is `4-7`,
   the housekeeping set `0-3`, and `nosmt` takes CPUs 8 to 15 offline, so the pins are
   `4,5,6,7`, one core each.

2. Register the runner under a user of its own. A job on this host runs whatever a
   branch or a dependency bump compiles, build scripts included, as the runner's user,
   so that user has no password, no sudo and no SSH key:

   ```sh
   # ignore: run on the bench host, not in this repository
   sudo useradd --create-home --shell /bin/bash runner
   sudo passwd -l runner
   ```

   In the organization's settings, under Actions, Runners, create a runner group for
   this repository alone, then add a new Linux x64 runner to it and follow the steps
   GitHub prints as the `runner` user (`sudo -u runner -H bash`), which download the
   runner into `/home/runner/actions-runner` and run `./config.sh` with a one-hour
   registration token. Give the runner the custom label `bench-linux`, which is the
   label the bench job names. Another host gets a label of its own, `grip-linux` for
   the host `grip`, and never `bench-linux`: the gate job
   selects that label, and its baseline belongs to the host that recorded it. Then
   install it as a service so it starts at boot:

   ```sh
   # ignore: run on the bench host, not in this repository
   cd /home/runner/actions-runner
   sudo ./svc.sh install runner
   sudo ./svc.sh start
   ```

   The runner appears as Idle on the organization's runner page. To move a runner
   that was registered under another user, stop it, `sudo ./svc.sh uninstall`, move
   the directory, `chown -R runner:runner` it, fix the home directory in its `.path`
   file, and install the service again; the registration survives the move.

3. Tune the host. Copy `bench/host/` from this repository to the host and run the
   setup as root, naming the sets from step 1:

   ```sh
   # ignore: run on the bench host, not in this repository (grip: ISOLATED=4-7 HOUSEKEEPING=0-3)
   sudo ISOLATED=8-15 HOUSEKEEPING=0-7 bash bench/host/setup-ubuntu.sh
   sudo reboot
   ```

   The script writes a GRUB drop-in that isolates the set and takes the scheduler tick,
   RCU callbacks, interrupts, deep sleep states, the lockup watchdog, SMT and transparent
   huge pages off it; a boot-time service that sets the performance governor and turns
   boost off; a drop-in that keeps the runner on the housekeeping CPUs; swap off;
   unattended upgrades off; and the twelve timers a stock Ubuntu Server starts jobs
   from masked, `sysstat-collect` every ten minutes among them, which on 2026-10-30
   fired 85 seconds into a bench and moved its unchecked concurrent claim rows by a
   tenth (card 170); `logrotate` and `fstrim` stay; and a boot-time service that binds
   every device interrupt a driver bound itself to the housekeeping CPUs, since the
   kernel line's `irqaffinity` does not reach those: Intel's Wi-Fi driver gives each
   receive queue one CPU, the isolated ones included, and on `grip` the runner's own
   traffic during a job fired them there, 600 to 900 times a bench, which read as one
   one_way run in about twenty at 17 to 64 µs (card 159). Every file it writes names
   it, and its header says how to undo each one. The kernel command line applies at the reboot. On Intel the deep
   sleep states go through `intel_idle.max_cstate=1`, because the idle driver there
   does not read `processor.max_cstate`, and turbo through intel_pstate's `no_turbo`,
   because there is no `cpufreq/boost` file; the script sets both on every host, and
   both are inert on AMD, so running it again on `bench` changes nothing `bench`
   measures and its baseline stands.

4. Check the host. After the reboot:

   ```sh
   # ignore: run on the bench host, not in this repository
   bash bench/host/check.sh 8,10,12,14
   ```

   The four CPUs are the ones the bench job pins the publisher, the subscriber, the
   conductor and, in the UDP rows, the sender and the receiver to: one per core, inside
   the isolated set, with the SMT siblings offline.
   The script prints the host's state, including the turbo switch and the idle states
   the pinned CPUs may enter, how many timers are active and which interrupts a driver
   bound itself and where, and ends with `check.sh: ok`; it fails when turbo is on or
   an idle state deeper than C1 is enabled, the two settings a host loses when the
   service or the kernel line did not apply, when one of the timers the set-up script
   masks is active, and when a driver-bound interrupt names an isolated CPU, the two
   findings of a host that runs an older version of the script. The bench job runs the same check first,
   so a host that lost its tuning fails the job before it produces a number.

5. Give the runner a toolchain. The bench job installs a stable toolchain and `just` on
   every run through the same actions as the other jobs, and installs `rustup` when the
   runner has none. A C linker is the one thing the actions do not bring; the setup
   script installs `build-essential` for it. Installing `rustup` for the `runner` user
   once and putting `/home/runner/.cargo/bin` first in the runner's `.path` file saves
   the install on every job.

6. Point the job at the host. The `bench (linux)` job in `.github/workflows/ci.yml`
   runs on `[self-hosted, bench-linux]` and pins to the CPUs from step 4; change the
   `--pin` list and the `check.sh` list together when the host's sets differ. Then add
   `bench (linux)` to the required checks of `master` in the repository's branch
   protection, beside `gates (linux)` and `gates (macos)`. The second host, `grip`, has
   a job of its own in the same file, `bench (grip)`, on `[self-hosted, grip-linux]`
   with its own lists, `4,5,6,7`, and its own profiles, `grip-linux` and
   `grip-linux-unchecked`, and is a required check too since ADR 0034's acceptance on
   2026-10-31, after it had been green over three pushes to `master` with its
   baselines in place; `bench (kip)`, the job of the host that left the gate, went
   from the file in the same change, and `kip`'s baselines stay in
   `bench/baseline.json` as its record, compared by nothing
   ([ADR 0034](../decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md)).

7. Record the baselines. The first run of the host's job reports every benchmark as
   `no baseline` and passes. A host's baselines are recorded from every process it has
   run at one tree, three at least
   ([ADR 0033](../decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)):
   re-run the job twice, with `gh run rerun <run id> --job <job id>` or the job's
   re-run button on the run's page, downloading each attempt's artifact before the
   next re-run, `bench-results-bench-linux` from `bench (linux)` on `bench` or
   `bench-results-grip-linux` from `bench (grip)` on `grip`, because a re-run replaces
   the run's artifact of the same name and only the newest attempt's is kept. Record
   each profile's
   files as [Run the bench](run-the-bench.md) describes in its step 4, under the
   host's own profiles, `bench-linux` and `bench-linux-unchecked` or `grip-linux` and
   `grip-linux-unchecked`, in one commit whose message holds the histograms. The
   artifact is uploaded whether or not the run passed the gate, so when the host has
   changed (Updating the host, below) the red runs after the change are the ones to
   record from.

## How to verify it worked

- `bash bench/host/check.sh 8,10,12,14` on `bench`, or `check.sh 4,5,6,7` on `grip`,
  ends with `check.sh: ok`.
- `cat /proc/interrupts` on the host shows the isolated CPUs' `LOC` column in the
  hundreds while the housekeeping CPUs' is in the hundreds of thousands.
- The organization's runner page shows the runner Idle after a reboot of the host.
- A push to `master` runs the host's job on it, `bench (linux)` on `bench` and
  `bench (grip)` on `grip`, and the run's log opens with the state `check.sh` printed,
  then the harness's line `pinned to CPUs 8,10,12,14` on `bench` or
  `pinned to CPUs 4,5,6,7` on `grip`. Once both are done, `bench (udp)` on `bench` and
  `bench (udp peer)` on `grip` run the link rows across the cable
  ([Run the bench](run-the-bench.md), step 5).

## Further hosts: the roles, the direct link and the clock

A second gate host is set up by the steps above with its own sets, its own runner label
and its own job (step 6); today that host is `grip`, label `grip-linux`, job
`bench (grip)`, pinned to CPUs 4, 5 and 6. The gate's hosts are the ones whose rows
hold still from one process to the next, because the gate compares a run with the
slowest run of three or more processes (ADR 0033), and a host whose medians wander
widens its own band; why `grip` took the role from `kip`, with the measurement, is
[ADR 0034](../decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md). `kip` is
the soak host, where a driver records for days (card 159): a soak host is not tuned,
so any machine serves it, and the gate gets the host that holds still.

A direct cable between the two tuned hosts carries the bench's link rows, which
[ADR 0044](../decisions/0044-gate-udp-on-the-two-bench-hosts.md) decides, and a
one-way number across hosts is only as good as the clock offset between them. The steps below
joined `bench` and `grip` on 2026-10-31, when the cable moved from `kip`'s `enp87s0`,
which had carried the first link since 2026-10-26, and were checked again on
2026-11-02 with no job on either host; the numbers are that record, and the full
record is on card 182.

1. One straight patch cable from a wired port with hardware timestamps on each host,
   port to port, with nothing between them. `ethtool -T <port>` lists
   `hardware-transmit`, `hardware-receive` and `hardware-raw-clock` on a port that
   qualifies, as it does on both of these; gigabit ports swap the pairs themselves, so
   no crossover cable is needed. Each host keeps its other link for SSH, CI and
   packages. On `bench` the port is `enp5s0` (Intel I211) and on `grip` `eno2` (Intel
   I219-LM); the link is 1000 Mb/s full duplex at both ends with an MTU of 1500, and
   `eno2`'s interrupt is bound to `grip`'s housekeeping CPUs by the set-up script's
   interrupt service, as every driver-bound interrupt is.

2. Static addresses on the link, no gateway, as a netplan drop-in on each host, applied
   to that port alone so the other link is not touched:

   ```sh
   # ignore: run on each host with its own port and address (bench: enp5s0 10.0.0.1/30; grip: eno2 10.0.0.2/30)
   sudo tee /etc/netplan/90-cavalry-link.yaml >/dev/null <<'YAML'
   network:
     version: 2
     ethernets:
       eno2:
         dhcp4: false
         dhcp6: false
         accept-ra: false
         addresses: [10.0.0.2/30]
         optional: true
   YAML
   sudo chmod 0600 /etc/netplan/90-cavalry-link.yaml
   sudo netplan generate && sudo networkctl reload && sudo networkctl reconfigure eno2
   ```

   `ping` from each end confirms the link, and `/etc/hosts` on both hosts names the
   ends `bench-link` (10.0.0.1) and `grip-link` (10.0.0.2), the names the bench's link
   jobs and the commands on this page use. On 2026-11-02, 100 pings each way lost none:
   `bench` to `grip` 0.089 ms at least, 0.123 ms on average and 0.213 ms at most, with
   a deviation of 0.023 ms; `grip` to `bench` 0.095, 0.117, 0.181 and 0.012 ms.

3. The clock. `bench` serves it and `grip` follows it over the link, both with hardware
   timestamps on the port, so the offset between the two hosts is bounded by the link
   and not by the internet:

   ```sh
   # ignore: run on bench, the host that serves the clock
   printf 'allow 10.0.0.2\nhwtimestamp enp5s0\n' | sudo tee /etc/chrony/conf.d/90-cavalry-link.conf
   sudo systemctl restart chrony
   ```

   ```sh
   # ignore: run on grip, the host that follows it
   printf 'server 10.0.0.1 iburst minpoll 0 maxpoll 2 prefer trust\nhwtimestamp eno2\n' | sudo tee /etc/chrony/conf.d/90-cavalry-link.conf
   sudo sed -E -i '/^pool /{s/ prefer$//; / noselect$/!s/$/ noselect/;}' /etc/chrony/sources.d/ubuntu-ntp-pools.sources
   sudo systemctl restart chrony
   ```

   On `grip`, `chronyc sources` marks `bench-link` with `^*` once selected and `^+`
   while it is combined with other sources, `chronyc sourcestats` gives the offset and
   its standard deviation against it, and `chronyc tracking` the offset and the root
   distance of the selected source; `chronyc clients` on `bench` lists `grip-link`. It
   worked when the standard deviation against `bench-link` is under 1 µs and the last
   offset is within a few microseconds: over this cable with hardware timestamps
   `grip` read 37 ns and a last offset of +2.0 µs fifty minutes after `trust` went on
   the `server` line, and 20 ns on 2026-11-02, with 245,499 NTP packets in `bench`'s
   `chronyc clients`. Without `trust` the host kept the internet pools, 3.3 µs and an
   offset of −852 µs, which is why the line carries it; the root distance of tens of
   milliseconds is `bench`'s own distance to UTC and does not enter a number between
   the two hosts. `tracking` reports the selected source, and the `sed` line is what
   makes that `bench-link`: Ubuntu's own pool lines carry `prefer` as well, and the
   pools are stratum 2 against `bench-link`'s 3, so without it `grip` selects a public
   pool server with `bench-link` combined, as it had on 2026-11-02, when the offset
   between the two hosts was the `bench-link` line of `sourcestats` and not
   `tracking`'s, and by 2026-11-04 that line read +388 µs with a standard deviation
   of 863 ns, the public servers' jitter steering the clock against the cable. With
   `noselect` on the five pool lines, which chrony keeps measuring and never follows,
   `bench-link` is the one source it can select, the change ADR 0044's Revisit
   names: a minute after `sudo chronyc reload sources` on 2026-11-04, `tracking` read
   `bench-link` as the reference with a system time 27 ns slow and a last offset of
   +50 ns, and `sourcestats` an offset of +2 ns with a standard deviation of 51 ns.
   With the link down `grip`'s clock runs free, which `chronyc sources` shows as
   `bench-link` unreachable; a host whose clock exists to be compared with `bench`'s
   has no better source to fall back on.

4. The firewall and the ports. `ufw` is inactive and no nftables ruleset is loaded on
   either host, so nothing is opened for the link. A host that enables one opens, on
   the link's interface, the UDP port its receive endpoint binds, 40182 on `bench` and
   40183 on `grip` for the link rows ADR 0044 decides, and admits the replies to the
   other host's sender: a receiver answers every status message and NAK to the
   datagram's source, which is the sender's own socket on a port the operating system
   chose, so a stateful firewall, `ufw` among them, lets the replies through by itself,
   and a stateless ruleset must open the reply path as well, or the stream never
   connects. A port a firewall drops shows as the publishing host's `cavalry doctor`
   saying `udp-publication-not-connected`
   ([Publish between two hosts](publish-between-two-hosts.md)).

## Access to the host

The administrative user reaches the host by SSH key only, with `PasswordAuthentication
no` in a drop-in under `/etc/ssh/sshd_config.d/`, and has passwordless sudo through a
one-line drop-in under `/etc/sudoers.d/`, so that an agent session can maintain the
host without holding a password. That is safe only because the runner's user is
separate: a passwordless administrator who also runs the jobs would hand root to every
build script.

## Updating the host

Packages and the kernel change only by hand, because a change underneath the baseline
reads as a regression nobody committed. Apply updates, reboot, run `check.sh`, then
re-record the host's two baselines from its job's next three runs, as step 7 says,
under its own profiles and from its own artifacts, in a commit whose message names
what changed on the host; a later run at the same tree that reads a row above them
joins them ([Run the bench](run-the-bench.md), step 4).

A later version of `setup-ubuntu.sh` reaches a host the same way: copy `bench/host/`
to it and run the script again with the host's sets. Every step writes a fixed value,
so only the new step changes anything; the timers step added on 2026-10-31 stops and
masks them with no reboot. A change under `bench/host/` ends a tree (ADR 0033), so the
baselines are re-recorded from the job's next three runs after it in either case.

## When it does not work

- `check.sh` reports `CPU N is not isolated`: the kernel command line did not apply.
  `cat /proc/cmdline` on the host; if `isolcpus` is missing, run `sudo update-grub`
  and reboot, and look for `90-cavalry-bench.cfg` under `/etc/default/grub.d/`.
- `check.sh` reports `governor is schedutil`: `cavalry-bench-host.service` did not
  run; `systemctl status cavalry-bench-host.service` on the host says why.
- `check.sh` reports `turbo is on`: the same service did not write intel_pstate's
  `no_turbo` or cpufreq's `boost`; `systemctl status cavalry-bench-host.service` says
  why, and `cat /sys/devices/system/cpu/intel_pstate/no_turbo` reads 1 once it has.
- `check.sh` reports `CPU N can enter an idle state with a N µs exit latency`: the
  kernel line lacks `intel_idle.max_cstate=1` or `processor.max_cstate=1`, or did not
  apply; `cat /proc/cmdline` on the host, then `sudo update-grub` and reboot.
- `check.sh` reports `irq N (name) is bound to isolated CPU`: `cavalry-bench-irqs.service`
  did not run after the device came up, or the host runs a version of the script from
  before it. `systemctl status cavalry-bench-irqs.service` on the host says which;
  `sudo /usr/local/sbin/cavalry-bench-irqs` moves the interrupts now, and the service
  does at every boot once the network is up. `cat /proc/interrupts` shows where each
  interrupt has fired since boot, so a vector that moved after firing on an isolated
  CPU still shows a count there; the affinity, not the count, is what the check reads.
- `check.sh` reports `timers active that setup-ubuntu.sh masks`: the host runs a version
  of the script from before its timers step, or a package upgrade re-enabled a timer
  that had been disabled rather than masked. Copy `bench/host/` to the host and run the
  script again (Updating the host, above); it stops and masks them without a reboot.
- The harness reports `could not bind the thread to CPU N`: the CPU is offline or
  outside the runner's allowed set. `cat /sys/devices/system/cpu/online` on the host,
  and remember that `nosmt` takes every odd CPU offline on this part.
- The job never starts and shows "Waiting for a runner": the runner is offline, or the
  runner group does not include this repository, or the job's labels do not match the
  runner's. The organization's runner page shows all three.
- The job runs on a pull request from a fork: it must not. The job's `if` line
  restricts it to pushes and to pull requests from this repository; a self-hosted runner
  on a public repository must never run a stranger's code.
