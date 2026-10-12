#!/usr/bin/env bash
# Print a Linux host's tuning state, and with a CPU list, fail unless the host is tuned
# for the latency gate on those CPUs (ADR 0016; docs/how-to/set-up-a-bench-host.md). The
# bench job runs this first, so every run's log names the host it ran on, and a host that
# lost its tuning fails before it produces a number.
#
#     bench/host/check.sh            print the state
#     bench/host/check.sh 8,10,12    print it and fail unless those CPUs are isolated and
#                                    tickless, SMT is off, the governor is performance,
#                                    boost is off, transparent huge pages are off and
#                                    swap is off
set -euo pipefail

cpu=/sys/devices/system/cpu
read_or() { if [ -r "$1" ]; then cat "$1"; else echo "$2"; fi; }

isolated=$(read_or $cpu/isolated "")
nohz_full=$(read_or $cpu/nohz_full "")
smt=$(read_or $cpu/smt/control unknown)
governor=$(sort -u $cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | paste -sd, - || echo unknown)
boost=$(read_or $cpu/cpufreq/boost none)
thp=$(read_or /sys/kernel/mm/transparent_hugepage/enabled unknown | sed -E 's/.*\[([a-z]+)\].*/\1/')
swap=$(swapon --show --noheadings 2>/dev/null | wc -l | tr -d ' ')
online=$(read_or $cpu/online unknown)

echo "bench host: $(hostname), $(uname -sr), $(grep -m1 'model name' /proc/cpuinfo | sed 's/.*: //')"
echo "  online CPUs:    $online"
echo "  isolated:       ${isolated:-none}"
echo "  nohz_full:      ${nohz_full:-none}"
echo "  smt:            $smt"
echo "  governor:       $governor"
echo "  boost:          $boost"
echo "  huge pages:     $thp"
echo "  swap devices:   $swap"
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
done
case "$smt" in off|forceoff|notsupported|notimplemented) ;; *) fail "SMT is $smt, not off" ;; esac
[ "$governor" = performance ] || fail "governor is $governor, not performance"
case "$boost" in 0|none) ;; *) fail "boost is on" ;; esac
[ "$thp" = never ] || fail "transparent huge pages are $thp, not never"
[ "$swap" = 0 ] || fail "$swap swap devices are active"
if [ "$failures" -gt 0 ]; then
    echo "check.sh: $failures finding(s); run bench/host/setup-ubuntu.sh as root and reboot (docs/how-to/set-up-a-bench-host.md)" >&2
    exit 1
fi
echo "check.sh: ok, CPUs $1 are isolated and tickless on a tuned host"
