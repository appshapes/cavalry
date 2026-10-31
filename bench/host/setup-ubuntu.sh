#!/usr/bin/env bash
# Tune an Ubuntu host for the latency gate (ADR 0016). docs/how-to/set-up-a-bench-host.md
# is the page; bench/host/check.sh verifies the result after the reboot this needs.
#
# Run as root, once, on a host that does nothing else. Every step writes a fixed file or
# value, so running it again changes nothing; a host takes a later version of this script
# by running it again (docs/how-to/set-up-a-bench-host.md, "Updating the host"). Undo:
# delete the files it names, `systemctl unmask` the timers of step 7, run `update-grub`,
# then reboot.
#
# The isolated CPUs carry the harness's three spinning threads and nothing else; the
# housekeeping CPUs carry the runner, the build, interrupts and every other process.
# The defaults are for an 8-core part with two core complexes of 4 cores and SMT, with
# siblings numbered 2n and 2n+1, such as the Ryzen 7 2700X: the second complex is
# isolated. Read `lscpu -e` and the L3 column before accepting them for another part.
# On a hybrid Intel part (P-cores and E-cores), /sys/devices/cpu_core/cpus names the
# P-cores: isolate three of them with their SMT siblings and leave the E-cores to
# housekeeping. On the i9-13900H (six P-cores at CPUs 0 to 11, siblings 2n and 2n+1,
# eight E-cores at 12 to 19) that is ISOLATED=6-11 HOUSEKEEPING=0-5,12-19 and
# `--pin 6,8,10`.
set -euo pipefail

ISOLATED="${ISOLATED:-8-15}"
HOUSEKEEPING="${HOUSEKEEPING:-0-7}"

if [ "$(id -u)" -ne 0 ]; then
    echo "setup-ubuntu.sh: run as root: sudo ISOLATED=$ISOLATED HOUSEKEEPING=$HOUSEKEEPING $0" >&2
    exit 1
fi

# 1. Kernel command line, as a GRUB drop-in so the distribution's file is untouched.
#    nosmt           one thread per core: a sibling shares the core's pipelines and caches
#    isolcpus        the scheduler never places anything there (domain) and managed
#                    interrupts stay away (managed_irq); the harness pins into the set
#    nohz_full       no scheduler tick on a core running one task: the 1 kHz tick is a
#                    microsecond stall every millisecond, 1,000 samples per second
#    rcu_nocbs       RCU callbacks run elsewhere
#    irqaffinity     every interrupt defaults to the housekeeping CPUs
#    processor.max_cstate=1  no deep sleep states: a wake from C2 or deeper costs tens of
#                    microseconds, and the spinning threads never sleep anyway
#    intel_idle.max_cstate=1  the same for Intel's idle driver, which does not read
#                    processor.max_cstate: without it an i9-13900H keeps C2 (127 µs
#                    exit) and C3 (1,048 µs) enabled on the isolated CPUs
#    tsc=reliable    keep the TSC as the clocksource; the clocksource watchdog otherwise
#                    runs a timer on every CPU and can demote it to a slow clock
#    nowatchdog      no lockup-detector timer on every CPU
#    skew_tick=1     the remaining ticks on the housekeeping CPUs do not fire together
#    transparent_hugepage=never  no compaction stalls; the log buffers are page-aligned
#                    files, not anonymous memory
cat > /etc/default/grub.d/90-cavalry-bench.cfg <<CFG
# Written by bench/host/setup-ubuntu.sh (Cavalry, ADR 0016). Delete, run update-grub and
# reboot to undo.
GRUB_CMDLINE_LINUX_DEFAULT="\$GRUB_CMDLINE_LINUX_DEFAULT nosmt isolcpus=domain,managed_irq,${ISOLATED} nohz_full=${ISOLATED} rcu_nocbs=${ISOLATED} irqaffinity=${HOUSEKEEPING} processor.max_cstate=1 intel_idle.max_cstate=1 tsc=reliable nowatchdog skew_tick=1 transparent_hugepage=never"
CFG
update-grub
echo "setup-ubuntu.sh: kernel command line written; it applies at the next boot"

# 2. A fixed clock: the performance governor and no boost, every boot, before the runner.
#    Boost moves the clock with temperature and with how many cores are busy; a fixed
#    clock is slower and repeatable, and repeatable is what a 5 percent rule needs.
cat > /usr/local/sbin/cavalry-bench-host-tune <<'TUNE'
#!/usr/bin/env bash
# Written by bench/host/setup-ubuntu.sh (Cavalry, ADR 0016); cavalry-bench-host.service runs it.
set -euo pipefail
for governor in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    echo performance > "$governor"
done
if [ -w /sys/devices/system/cpu/cpufreq/boost ]; then
    echo 0 > /sys/devices/system/cpu/cpufreq/boost
fi
# intel_pstate has no cpufreq/boost file; its turbo switch is no_turbo.
if [ -w /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
    echo 1 > /sys/devices/system/cpu/intel_pstate/no_turbo
fi
TUNE
chmod 755 /usr/local/sbin/cavalry-bench-host-tune
cat > /etc/systemd/system/cavalry-bench-host.service <<UNIT
# Written by bench/host/setup-ubuntu.sh (Cavalry, ADR 0016).
[Unit]
Description=Cavalry bench host: performance governor, no boost
DefaultDependencies=no
After=sysinit.target
Before=basic.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/sbin/cavalry-bench-host-tune

[Install]
WantedBy=basic.target
UNIT

# 3. The GitHub Actions runner, when one is installed, starts after the tuning and stays
#    on the housekeeping CPUs, as does everything it starts; the harness moves its own
#    threads onto the isolated CPUs with --pin.
for unit in /etc/systemd/system/actions.runner.*.service; do
    [ -e "$unit" ] || continue
    mkdir -p "$unit.d"
    cat > "$unit.d/cavalry-bench.conf" <<DROPIN
# Written by bench/host/setup-ubuntu.sh (Cavalry, ADR 0016).
[Unit]
After=cavalry-bench-host.service
Wants=cavalry-bench-host.service

[Service]
CPUAffinity=${HOUSEKEEPING}
DROPIN
    echo "setup-ubuntu.sh: runner $(basename "$unit") pinned to CPUs ${HOUSEKEEPING}"
done
systemctl daemon-reload
systemctl enable --now cavalry-bench-host.service >/dev/null
echo "setup-ubuntu.sh: governor $(sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor | paste -sd, -), boost $(cat /sys/devices/system/cpu/cpufreq/boost 2>/dev/null || echo none), intel_pstate no_turbo $(cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || echo none)"

# 4. No swap: a page-out is a stall of milliseconds, and the host has the memory.
swapoff -a || true
sed -i -E '/^[^#].*[[:space:]]swap[[:space:]]/ s/^/# cavalry-bench: /' /etc/fstab
echo "setup-ubuntu.sh: swap off ($(swapon --show --noheadings | wc -l) devices active)"

# 5. No unattended changes to the platform: a kernel or a libc that moves under the
#    baseline is a regression nobody committed. Updates are applied by hand, and the
#    baseline is re-recorded afterwards (docs/how-to/set-up-a-bench-host.md).
cat > /etc/apt/apt.conf.d/99cavalry-bench <<APT
// Written by bench/host/setup-ubuntu.sh (Cavalry, ADR 0016): a bench host changes by hand only.
APT::Periodic::Update-Package-Lists "0";
APT::Periodic::Unattended-Upgrade "0";
APT
systemctl disable --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer >/dev/null 2>&1 || true
echo "setup-ubuntu.sh: unattended upgrades off"

# 6. What a Rust build needs that a server install lacks: a C linker.
export DEBIAN_FRONTEND=noninteractive
apt-get update -q >/dev/null
apt-get install -y -q build-essential >/dev/null
echo "setup-ubuntu.sh: $(cc --version | head -1)"

# 7. No scheduled jobs: a timer's service runs on the housekeeping CPUs while a bench may
#    be on the isolated ones, and on 2026-10-30 sysstat-collect.service, which its timer
#    starts every ten minutes, fired 85 seconds into a bench whose unchecked concurrent
#    claim rows then read a tenth slower with the whole row moved, p50 to p99.99 (card
#    170). A stock Ubuntu Server runs these twelve on a schedule and a bench host needs
#    none of them. Masked, not disabled: a package upgrade re-enables a disabled unit
#    and leaves a masked one alone. Two stay: logrotate.timer, because rsyslog writes
#    /var/log/syslog and nothing else bounds it, and fstrim.timer, for the drive; both
#    run at midnight UTC, daily and weekly. check.sh fails when one of the twelve is
#    active. Undo: `systemctl unmask <timer>.timer`.
for timer in sysstat-collect sysstat-rotate sysstat-summary fwupd-refresh \
        update-notifier-download update-notifier-motd motd-news man-db dpkg-db-backup \
        systemd-tmpfiles-clean e2scrub_all xfs_scrub_all; do
    systemctl stop "$timer.timer" >/dev/null 2>&1 || true
    systemctl mask "$timer.timer" >/dev/null 2>&1 || true
done
echo "setup-ubuntu.sh: timers masked: $(systemctl list-unit-files --type=timer --state=masked --no-legend | awk '{print $1}' | sed 's/\.timer$//' | paste -sd, -)"
echo "setup-ubuntu.sh: timers still active: $(systemctl list-timers --no-legend | awk '{print $(NF-1)}' | sed 's/\.timer$//' | paste -sd, -)"

echo "setup-ubuntu.sh: done; reboot, then run bench/host/check.sh with the CPUs the harness will pin"
