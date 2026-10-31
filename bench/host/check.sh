#!/usr/bin/env bash
# Print a Linux host's tuning state, and with a CPU list, fail unless the host is tuned
# for the latency gate on those CPUs (ADR 0016; docs/how-to/set-up-a-bench-host.md). The
# bench job runs this first, so every run's log names the host it ran on, and a host that
# lost its tuning fails before it produces a number.
#
#     bench/host/check.sh            print the state
#     bench/host/check.sh 8,10,12    print it and fail unless those CPUs are isolated and
#                                    tickless, SMT is off, the governor is performance,
#                                    turbo is off (cpufreq boost, or intel_pstate's
#                                    no_turbo), no idle state deeper than C1 is enabled
#                                    on those CPUs, transparent huge pages are off,
#                                    swap is off and none of the timers setup-ubuntu.sh
#                                    masks is active
set -euo pipefail

cpu=/sys/devices/system/cpu
read_or() { if [ -r "$1" ]; then cat "$1"; else echo "$2"; fi; }

isolated=$(read_or $cpu/isolated "")
nohz_full=$(read_or $cpu/nohz_full "")
smt=$(read_or $cpu/smt/control unknown)
governor=$(sort -u $cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | paste -sd, - || echo unknown)
boost=$(read_or $cpu/cpufreq/boost none)
no_turbo=$(read_or $cpu/intel_pstate/no_turbo none)
# Turbo is cpufreq's boost switch on AMD and intel_pstate's no_turbo on Intel.
case "$boost:$no_turbo" in
    *:1) turbo=off ;;
    *:0) turbo=on ;;
    1:*) turbo=on ;;
    0:*) turbo=off ;;
    *) turbo=none ;;
esac
thp=$(read_or /sys/kernel/mm/transparent_hugepage/enabled unknown | sed -E 's/.*\[([a-z]+)\].*/\1/')
swap=$(swapon --show --noheadings 2>/dev/null | wc -l | tr -d ' ')
online=$(read_or $cpu/online unknown)
# The timers setup-ubuntu.sh (step 7) masks, the same list: one that is active on a tuned
# host is a job that will run on it during a bench (card 170).
masked_timers="sysstat-collect sysstat-rotate sysstat-summary fwupd-refresh \
    update-notifier-download update-notifier-motd motd-news man-db dpkg-db-backup \
    systemd-tmpfiles-clean e2scrub_all xfs_scrub_all"
active_timers=""
for timer in $masked_timers; do
    [ "$(systemctl is-active "$timer.timer" 2>/dev/null)" = active ] && active_timers="$active_timers $timer"
done
active_timers=${active_timers# }
timers_total=$(systemctl list-timers --no-legend 2>/dev/null | grep -c . || true)

# The idle states a CPU may enter, "name(exit latency in µs)" for each enabled one, and
# the deepest enabled exit latency. C1 exits in a microsecond or two; C2 and deeper in
# hundreds (an i9-13900H: C2 127 µs, C3 1,048 µs), the stall the kernel line removes
# with processor.max_cstate=1 (acpi_idle) and intel_idle.max_cstate=1 (intel_idle).
idle_states() {
    local s
    for s in $cpu/cpu$1/cpuidle/state*; do
        [ -r "$s/disable" ] && [ "$(cat "$s/disable")" = 0 ] || continue
        printf '%s(%s) ' "$(cat "$s/name")" "$(cat "$s/latency")"
    done
}
deepest_idle_latency() {
    local s lat max=0
    for s in $cpu/cpu$1/cpuidle/state*; do
        [ -r "$s/disable" ] && [ "$(cat "$s/disable")" = 0 ] || continue
        lat=$(cat "$s/latency")
        [ "$lat" -gt "$max" ] && max=$lat
    done
    echo "$max"
}
first=${1:-0}; first=${first%%,*}; first=${first%%-*}

echo "bench host: $(hostname), $(uname -sr), $(grep -m1 'model name' /proc/cpuinfo | sed 's/.*: //')"
echo "  online CPUs:    $online"
echo "  isolated:       ${isolated:-none}"
echo "  nohz_full:      ${nohz_full:-none}"
echo "  smt:            $smt"
echo "  governor:       $governor"
echo "  turbo:          $turbo (cpufreq boost $boost, intel_pstate no_turbo $no_turbo)"
echo "  idle states:    $(idle_states "$first")on CPU $first"
echo "  huge pages:     $thp"
echo "  swap devices:   $swap"
echo "  timers:         $timers_total active${active_timers:+, of the masked set: $active_timers}"
echo "  cmdline:        $(cat /proc/cmdline)"

[ $# -ge 1 ] || exit 0

# Expand a kernel CPU list, "8-15,20", one CPU per line.
expand() {
    local part
    IFS=, read -ra parts <<<"$1"
    for part in "${parts[@]}"; do
        case "$part" in
            "") ;;
            *-*) seq "${part%-*}" "${part#*-}" ;;
            *) echo "$part" ;;
        esac
    done
}

failures=0
fail() { echo "check.sh: $1" >&2; failures=$((failures + 1)); }
for pin in $(expand "$1"); do
    expand "$isolated" | grep -qx "$pin" || fail "CPU $pin is not isolated (isolcpus)"
    expand "$nohz_full" | grep -qx "$pin" || fail "CPU $pin is not tickless (nohz_full)"
    latency=$(deepest_idle_latency "$pin")
    [ "$latency" -le 10 ] || fail "CPU $pin can enter an idle state with a $latency µs exit latency ($(idle_states "$pin")); add processor.max_cstate=1 and intel_idle.max_cstate=1 to the kernel line and reboot"
done
case "$smt" in off|forceoff|notsupported|notimplemented) ;; *) fail "SMT is $smt, not off" ;; esac
[ "$governor" = performance ] || fail "governor is $governor, not performance"
[ "$turbo" != on ] || fail "turbo is on (cpufreq boost $boost, intel_pstate no_turbo $no_turbo); cavalry-bench-host.service did not run"
[ "$thp" = never ] || fail "transparent huge pages are $thp, not never"
[ "$swap" = 0 ] || fail "$swap swap devices are active"
[ -z "$active_timers" ] || fail "timers active that setup-ubuntu.sh masks: $active_timers; run it again (docs/how-to/set-up-a-bench-host.md, Updating the host)"
if [ "$failures" -gt 0 ]; then
    echo "check.sh: $failures finding(s); run bench/host/setup-ubuntu.sh as root and reboot (docs/how-to/set-up-a-bench-host.md)" >&2
    exit 1
fi
echo "check.sh: ok, CPUs $1 are isolated and tickless on a tuned host"
