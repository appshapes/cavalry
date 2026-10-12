# Set up a bench host for the latency gate

A how-to page: one task, turning a Linux machine into the host that runs the latency
gate, `just bench` and `just bench-compare`, as a self-hosted GitHub Actions runner. Why
the gate runs on a dedicated host and not on a shared runner is
[ADR 0016](../decisions/0016-run-the-latency-gate-on-a-dedicated-host.md); how to run
the harness and read its table is [Run the bench](run-the-bench.md).

## Prerequisites

- A machine that will do nothing else. The gate is a 5 percent rule on p99.99, and one
  other process on the measured cores moves p99.99 by more than that. An 8-core desktop
  part is enough; the first host is an AMD Ryzen 7 2700X with 60 GiB.
- Ubuntu Server, 24.04 LTS or later, installed with one administrative user and SSH.
  The scripts use systemd, GRUB and apt and have run on 26.04.1.
- Administrator rights on the GitHub organization, to register a runner and put it in
  a runner group, and on the repository, to add the job to the required checks.

## Steps

1. Choose the CPUs. On the host, run `lscpu -e` and read the `L3` column: CPUs that
   share an L3 cache are one core complex, and a line moved between two complexes costs
   about 100 ns more than one moved inside one. The harness needs three cores for its
   three spinning threads. Isolate one whole complex and leave the other to everything
   else. On the Ryzen 7 2700X, CPUs 0 to 7 are one complex and 8 to 15 the other, with
   each core's two SMT threads numbered 2n and 2n+1; the isolated set is `8-15` and
   the housekeeping set `0-7`, which are the scripts' defaults.

2. Register the runner. In the organization's settings, under Actions, Runners, create
   a runner group for this repository alone, then add a new Linux x64 runner to it and
   follow the steps GitHub prints, which download the runner into a directory of the
   administrative user and run `./config.sh` with a one-hour registration token. Give
   the runner the custom label `bench-linux`, which is the label the bench job names.
   Then install it as a service so it starts at boot:

   ```sh
   # ignore: run on the bench host, not in this repository
   cd ~/actions-runner
   sudo ./svc.sh install "$USER"
   sudo ./svc.sh start
   ```

   The runner appears as Idle on the organization's runner page.

3. Tune the host. Copy `bench/host/` from this repository to the host and run the
   setup as root, naming the sets from step 1:

   ```sh
   # ignore: run on the bench host, not in this repository
   sudo ISOLATED=8-15 HOUSEKEEPING=0-7 bash bench/host/setup-ubuntu.sh
   sudo reboot
   ```

   The script writes a GRUB drop-in that isolates the set and takes the scheduler tick,
   RCU callbacks, interrupts, deep sleep states, the lockup watchdog, SMT and transparent
   huge pages off it; a boot-time service that sets the performance governor and turns
   boost off; a drop-in that keeps the runner on the housekeeping CPUs; swap off; and
   unattended upgrades off. Every file it writes names it, and its header says how to
   undo each one. The kernel command line applies at the reboot.

4. Check the host. After the reboot:

   ```sh
   # ignore: run on the bench host, not in this repository
   bash bench/host/check.sh 8,10,12
   ```

   The three CPUs are the ones the bench job pins the publisher, the subscriber and the
   conductor to: one per core, inside the isolated set, with the SMT siblings offline.
   The script prints the host's state and ends with `check.sh: ok`. The bench job runs
   the same check first, so a host that lost its tuning fails the job before it produces
   a number.

5. Give the runner a toolchain. The bench job installs a stable toolchain and `just` on
   every run through the same actions as the other jobs, and installs `rustup` when the
   runner has none. A C linker is the one thing the actions do not bring; the setup
   script installs `build-essential` for it.

6. Point the job at the host. The `bench (linux)` job in `.github/workflows/ci.yml`
   runs on `[self-hosted, bench-linux]` and pins to the CPUs from step 4; change the
   `--pin` list and the `check.sh` list together when the host's sets differ. Then add
   `bench (linux)` to the required checks of `master` in the repository's branch
   protection, beside `gates (linux)` and `gates (macos)`.

7. Record the baseline. The first run on the host reports every benchmark as
   `no baseline` and passes. Download its `bench-results-bench-linux` artifact and
   record it as [Run the bench](run-the-bench.md) describes, under the profiles
   `bench-linux` and `bench-linux-unchecked`, in one commit whose message holds the
   histograms.

## How to verify it worked

- `bash bench/host/check.sh 8,10,12` on the host ends with `check.sh: ok`.
- `cat /proc/interrupts` on the host shows the isolated CPUs' `LOC` column in the
  hundreds while the housekeeping CPUs' is in the hundreds of thousands.
- The organization's runner page shows the runner Idle after a reboot of the host.
- A push to `master` runs `bench (linux)` on it, and the run's log opens with the
  state `check.sh` printed, then the harness's line `pinned to CPUs 8,10,12`.

## Updating the host

Packages and the kernel change only by hand, because a change underneath the baseline
reads as a regression nobody committed. Apply updates, reboot, run `check.sh`, then
re-record the baseline as step 7 says, in a commit whose message names what changed on
the host.

## When it does not work

- `check.sh` reports `CPU N is not isolated`: the kernel command line did not apply.
  `cat /proc/cmdline` on the host; if `isolcpus` is missing, run `sudo update-grub`
  and reboot, and look for `90-cavalry-bench.cfg` under `/etc/default/grub.d/`.
- `check.sh` reports `governor is schedutil`: `cavalry-bench-host.service` did not
  run; `systemctl status cavalry-bench-host.service` on the host says why.
- The harness reports `could not bind the thread to CPU N`: the CPU is offline or
  outside the runner's allowed set. `cat /sys/devices/system/cpu/online` on the host,
  and remember that `nosmt` takes every odd CPU offline on this part.
- The job never starts and shows "Waiting for a runner": the runner is offline, or the
  runner group does not include this repository, or the job's labels do not match the
  runner's. The organization's runner page shows all three.
- The job runs on a pull request from a fork: it must not. The job's `if` line
  restricts it to pushes and to pull requests from this repository; a self-hosted runner
  on a public repository must never run a stranger's code.
