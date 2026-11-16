#!/usr/bin/env bash
# The rolling upgrade gate: CLAUDE.md, invariant 3, "Mixed versions run during an
# upgrade", whose gate is "`conformance/upgrade/` runs N and N+1 together" (work item
# 231; ADR 0136). It upgrades a cluster of three members on the loopback from one build
# of Cavalry, N, to another, N+1, one member at a time, while a client adds numbers. It
# fails unless every number the cluster acknowledged is in every member's log at the
# end, and the three logs are one log. The rule it holds is spec/cluster.md, "Versions
# and the upgrade": members of two versions of one major are one cluster.
#
# The two builds
#
# N+1 is this tree as it stands, built into target/release. N is a commit:
#
# - CAVALRY_UPGRADE_FROM, when it is set: a hash, a tag or a branch.
# - Otherwise the last tree of the version before this one, read from the history. A
#   version is the first line of spec/cluster.md, "# Cluster protocol, version <n>".
#   The script takes the commits of HEAD's first-parent history that changed
#   spec/cluster.md, newest first (`git rev-list --first-parent HEAD --
#   spec/cluster.md`), and stops at the first whose first parent's page begins with
#   another line than HEAD's page: that commit moved the version, and N is its first
#   parent. A first parent that has no spec/cluster.md is passed over. When no commit
#   is so, the script fails and says which of two causes it is: a history that is
#   shallow, which is fetched whole (`git fetch --unshallow`; fetch-depth: 0 in
#   actions/checkout), or a version line that never moved, which needs
#   CAVALRY_UPGRADE_FROM.
#
# N's tree is exported with `git archive` to target/upgrade/<N's first 8 hex
# digits>/src and built into target/upgrade/<the same>/target. The hash names the tree,
# so a directory that holds the exported tree and the three programs is a cache, and N
# is not exported or built again; remove target/upgrade to build it again. Each build
# is the binary and two examples, `cavalry`, `adder-service` and `adder-client`, as
# scripts/run-a-cluster.sh builds them. N's build is kept on a host that keeps target/,
# as a laptop does. CI's cache drops built programs: Swatinem/rust-cache keeps only
# build/, .fingerprint/ and deps/ of each profile directory under target/ when it saves
# (its src/cleanup.ts, cleanTargetDir), so the three programs are never restored, and
# CI's gates job builds N on every run and prints `built N in <n> s` each time.
#
# The schedule
#
# A program and the driver it is a client of are always of one build: what passes
# between them is spec/client-protocol.md's, not the cluster protocol's. So a member
# is upgraded with the service beside it, and the client with its driver.
#
# a. Three members start at N, with N's adder-service beside each and a driver at N
#    for the client. They elect a leader. N's adder-client adds two numbers.
# b. A follower is upgraded: its service and then the member are stopped with SIGTERM,
#    and both start from N+1 on the directories they had. It follows the same leader in
#    the same term. N's adder-client adds two numbers. Two members are at N, one at N+1.
# c. The leader, at N, is stopped with its service. The two that remain, one at N and
#    one at N+1, elect a leader: the ballot crosses the two builds. The client's driver
#    is upgraded, and N+1's adder-client adds two numbers. The stopped member and its
#    service start from N+1 and follow the leader.
# d. The last member at N is upgraded as in b. When it led, it is started again at
#    once, as an upgrade starts it, and the three elect a leader, all at N+1. N+1's
#    adder-client adds two numbers.
# e. The checks of the whole log, below. Then every member is stopped, and a driver at
#    N+1 that is no member replays each member's log from its log directory.
# f. Everything the script started has ended. The last line is `conformance/upgrade:
#    ok`, and the exit is 0.
#
# What it asserts
#
# - After every start and stop, `cavalry doctor` on each running member, asked of the
#   member's own build: one member says `the leader in term <t>, election closed`,
#   every other says `a follower in term <t>, election closed` and `it follows member
#   <the leader>`, and all report one position as append position and commit position.
#   A follower's start moves neither the leader nor the term.
# - At the same moments, `cavalry doctor` of N+1 on each running member, whatever build
#   the member is of: the `note cluster-member` line ends with what the member's own
#   counter `cluster.protocol_version` gives (ADR 0137), and the script holds that to
#   the build it started the member from. A member at N+1 is named `it runs version
#   <n> of the cluster protocol, 0.<n>.0`, where <n> is read from the first line of
#   this tree's spec/cluster.md. Whether a member at N has the counter is read from
#   N's counters.toml alone, in N's exported tree, target/upgrade/<hash>/src, and not
#   from the version of N's page: every tree of version 0 lacks it, and so does the
#   first tree of version 1. When N's counters.toml holds no such counter, doctor
#   must say `it has no cluster.protocol_version, so it runs a build older than that
#   counter: version 0 of the cluster protocol, or the first build of version 1`.
#   When it holds it, a member at N is named as one at N+1 is, by the first line of
#   the page in N's exported tree. Each step's line says what doctor named, and `with
#   no cluster.protocol_version` of a member that has none.
# - After every run of the client: the cluster answered each number with the running
#   total, which the script keeps; the service beside every running member printed that
#   number with that total; and the leader's service alone replied.
# - A service that starts at N+1 reads the log that N wrote from its start and reaches
#   the same total.
# - At the end the last total each service printed is the client's total, and the
#   three services printed the same entries: positions, terms, sessions and totals.
# - Each member's log replays to the same bytes: its recordings are replayed in the
#   order its file `log-recordings-<member id>` gives (spec/cluster.md, "The log's
#   recordings"), as scripts/replay-check.sh replays three members' logs, and the three
#   outputs must hash the same, end at the commit position the members reported, and
#   hold the same number of messages.
#
# By hand
#
#   conformance/upgrade/run.sh                                  N from the history
#   CAVALRY_UPGRADE_FROM=<commit> conformance/upgrade/run.sh    N as named
#   CAVALRY_UPGRADE_FROM=HEAD conformance/upgrade/run.sh        this tree's changes over its last commit
#
# The script's directories are under one made by `mktemp`, in /dev/shm when it exists
# and the temporary directory otherwise. Its sixteen ports are counted out of 10000 to
# 19999 from the shell's process id, as conformance/c/run.sh counts its own. Every
# wait of the script is bounded at 60 s and says what it waited for. The adder-client
# bounds its own: 20 s for a leader to open its session, 10 s for an offer and 20 s
# for a reply (crates/cavalry/examples/adder-client.rs). On a failure the script
# prints the step and the reason, what `cavalry doctor` says of each member that
# still runs, and each program's output; it stops what it started, by process id,
# leaves its directory and says where, and exits 1. It runs under bash 3.2, which
# macOS has, and under bash 5.
set -euo pipefail

cd "$(dirname "$0")/../.."
repo=$(pwd)
began=$(date +%s)
# A member is started by these two keys (docs/how-to/run-a-cluster.md). The script sets
# them for each member it starts and for nothing else, whatever the caller's
# environment holds.
unset CAVALRY_CLUSTER_MEMBERS CAVALRY_CLUSTER_MEMBER_ID

step="1 (name the older build)"
root=""
members=""
# The processes the script started that have not ended, by process id.
pids=""
reported=""
# By member id: the member's process, its service's process, and the build both are
# of, `older` for N and `newer` for N+1. The two processes are empty while the member
# is stopped.
member_pid=("" "" "")
service_pid=("" "" "")
member_build=("" "" "")
client_driver_pid=""

say() { printf 'conformance/upgrade: %s\n' "$1"; }
# Fails the step with why. Called from the script's own shell alone, never from a
# function whose output is captured: an exit there would end the capture and not the
# script.
fail() {
    printf 'conformance/upgrade: step %s failed: %s\n' "$step" "$1" >&2
    reported=yes
    exit 1
}
# The path of program `$2`, `cavalry`, `adder-service` or `adder-client`, in build
# `$1`, `older` or `newer`.
program() {
    local release
    if [ "$1" = older ]; then release="$older/target/release"; else release="$repo/target/release"; fi
    if [ "$2" = cavalry ]; then echo "$release/cavalry"; else echo "$release/examples/$2"; fi
}
# What a build is called in what the script prints.
named() { if [ "$1" = older ]; then echo "N"; else echo "N+1"; fi; }
# Each member with its build, for a step's line.
builds() {
    local id text=""
    for id in 0 1 2; do
        if [ -n "${member_pid[$id]}" ]; then
            text="${text:+$text, }member $id at $(named "${member_build[$id]}")"
        else
            text="${text:+$text, }member $id stopped"
        fi
    done
    echo "$text"
}
# Takes process `$1` out of the list of those that run.
forget() {
    local pid kept=""
    for pid in $pids; do
        [ "$pid" = "$1" ] || kept="$kept $pid"
    done
    pids=$kept
}
# What `cavalry doctor` says of member `$1`, whole, asked of the build the member runs.
doctor() {
    CAVALRY_CLUSTER_MEMBERS=$members CAVALRY_CLUSTER_MEMBER_ID=$1 "$(program "${member_build[$1]}" cavalry)" doctor \
        --dir "$root/member-$1" --log-dir "$root/member-$1-log" 2>&1 || true
}
# What `cavalry doctor` of N+1 says of member `$1`, whole, whatever build the member
# runs: the doctor that names a member's version of the cluster protocol (ADR 0137).
doctor_newer() {
    CAVALRY_CLUSTER_MEMBERS=$members CAVALRY_CLUSTER_MEMBER_ID=$1 "$(program newer cavalry)" doctor \
        --dir "$root/member-$1" --log-dir "$root/member-$1-log" 2>&1 || true
}
# `member 1`, `members 0 and 2` or `members 0, 1 and 2`, for the ids `$1`.
members_text() {
    # shellcheck disable=SC2086  # the ids are split into words on purpose
    set -- $1
    case $# in
        1) echo "member $1" ;;
        2) echo "members $1 and $2" ;;
        *) echo "members $1, $2 and $3" ;;
    esac
}

# At any exit. After a failure: the step, when nothing said it yet, doctor on each
# member that still runs, and every program's output. Always: every process the script
# started and that still runs is ended, by its process id, with SIGTERM and, 10 s
# later, SIGKILL. The directory goes after a clean run and stays after a failure.
finish() {
    local status=$? id pid out deadline
    trap - EXIT
    set +e
    if [ "$status" != 0 ]; then
        [ -n "$reported" ] || printf 'conformance/upgrade: step %s failed: a command of the script exited %s, and its own message is above\n' "$step" "$status" >&2
        for id in 0 1 2; do
            pid=${member_pid[$id]}
            if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
                printf -- '--- cavalry doctor on member %s, at %s\n' "$id" "$(named "${member_build[$id]}")" >&2
                doctor "$id" >&2
            fi
        done
        if [ -n "$root" ]; then
            for out in "$root"/*.out; do
                [ -f "$out" ] || continue
                printf -- '--- %s\n' "$(basename "$out")" >&2
                cat "$out" >&2
            done
        fi
    fi
    for pid in $pids; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null
            deadline=$((SECONDS + 10))
            while kill -0 "$pid" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do sleep 0.1; done
            if kill -0 "$pid" 2>/dev/null; then kill -KILL "$pid" 2>/dev/null; fi
            wait "$pid" 2>/dev/null
        fi
    done
    if [ -n "$root" ]; then
        if [ "$status" = 0 ]; then
            rm -rf "$root"
        else
            printf 'conformance/upgrade: the directories and every program'\''s output are left in %s; remove it when you have read them\n' "$root" >&2
        fi
    fi
    [ "$status" = 0 ] || exit 1
}
trap finish EXIT
trap 'reported=yes; printf "conformance/upgrade: step %s was interrupted\n" "$step" >&2; exit 1' HUP INT TERM

# Waits for process `$1`, which the script started and `$2` names, to end: 60 s at
# most. The shell collects a child that ends, so `kill -0` fails from then on.
ended() {
    local deadline=$((SECONDS + 60))
    while kill -0 "$1" 2>/dev/null; do
        [ "$SECONDS" -lt "$deadline" ] || fail "waited 60 s for $2 (pid $1) to end"
        sleep 0.1
    done
}
# Stops process `$1`, which the script started and `$2` names, as an operator does:
# SIGTERM, and then its end.
stop() {
    kill -TERM "$1" 2>/dev/null || true
    ended "$1" "$2, after its SIGTERM,"
    wait "$1" 2>/dev/null || true
    forget "$1"
}
# Waits until file `$1` holds a line that matches `$2`, 60 s at most; `$3` is what is
# waited for, and `$4` the process that writes the file, whose end ends the wait.
await() {
    local deadline=$((SECONDS + 60))
    until grep -q -- "$2" "$1" 2>/dev/null; do
        if ! kill -0 "$4" 2>/dev/null; then
            # Its last line may have come as it ended.
            if grep -q -- "$2" "$1" 2>/dev/null; then return 0; fi
            fail "waited for $3, and the program (pid $4) ended first; it said: $(cat "$1" 2>/dev/null)"
        fi
        [ "$SECONDS" -lt "$deadline" ] || fail "waited 60 s for $3"
        sleep 0.1
    done
}

# The commit that is the last tree of the version before HEAD's, as the header says;
# fails when the history holds none.
version_before() {
    local page=spec/cluster.md now line commit parent
    now=$(git show "HEAD:$page" | sed -n 1p)
    for commit in $(git rev-list --first-parent HEAD -- "$page"); do
        # No first parent: the history's first commit, or the edge of a shallow one.
        parent=$(git rev-parse --verify --quiet "$commit^1") || continue
        # No page in the first parent: a tree before the cluster protocol.
        line=$(git show "$parent:$page" 2>/dev/null | sed -n 1p) || continue
        if [ "$line" != "$now" ]; then
            echo "$parent"
            return 0
        fi
    done
    return 1
}

if [ -n "${CAVALRY_UPGRADE_FROM:-}" ]; then
    from=$(git rev-parse --verify --quiet "$CAVALRY_UPGRADE_FROM^{commit}") \
        || fail "CAVALRY_UPGRADE_FROM is \`$CAVALRY_UPGRADE_FROM\`, which names no commit of this repository; give a hash, a tag or a branch that \`git rev-parse\` resolves, or unset it to take N from the history"
    how="named by CAVALRY_UPGRADE_FROM"
elif from=$(version_before); then
    how="the first parent of the commit that last moved the first line of spec/cluster.md"
elif [ "$(git rev-parse --is-shallow-repository)" = true ]; then
    fail "this repository's history is shallow, and no commit it holds moves the first line of spec/cluster.md, so the last tree of the version before this one is not here; fetch the whole history, \`git fetch --unshallow\`, or fetch-depth: 0 in the workflow's actions/checkout, and run this again; or name N yourself with CAVALRY_UPGRADE_FROM=<commit>"
else
    fail "the first line of spec/cluster.md, \`$(git show HEAD:spec/cluster.md | sed -n 1p)\`, is the same in every commit of HEAD's first-parent history, so this tree's version has no version before it; name N yourself: CAVALRY_UPGRADE_FROM=<commit> conformance/upgrade/run.sh, where CAVALRY_UPGRADE_FROM=HEAD runs this tree's changes over its last commit"
fi
short=${from:0:8}
older="$repo/target/upgrade/$short"
say "N is $short, $how: $(git log -1 --format=%s "$from")"
say "N's spec/cluster.md begins \`$(git show "$from:spec/cluster.md" 2>/dev/null | sed -n 1p)\`; N+1 is this tree, whose page begins \`$(sed -n 1p spec/cluster.md)\`"

step="2 (build N+1, this tree)"
at=$(date +%s)
cargo build --release -p cavalry --bin cavalry --example adder-service --example adder-client
say "built N+1 in $(( $(date +%s) - at )) s, into target/release"

step="2 (build N, $short)"
if [ -d "$older/src" ] && [ -x "$(program older cavalry)" ] && [ -x "$(program older adder-service)" ] && [ -x "$(program older adder-client)" ]; then
    say "N is built: target/upgrade/$short holds its exported tree and its three programs, and the hash names the tree, so they are not built again (0 s)"
else
    at=$(date +%s)
    rm -rf "$older/src"
    mkdir -p "$older/src"
    git archive --format=tar "$from" | tar -x -f - -C "$older/src"
    (cd "$older/src" && cargo build --release -p cavalry --bin cavalry --example adder-service --example adder-client \
        --target-dir "$older/target") \
        || fail "N, $short, did not build from target/upgrade/$short/src; cargo's own message is above. N must be a tree that has the \`cavalry\` binary and the examples adder-service and adder-client; name another with CAVALRY_UPGRADE_FROM"
    for built in cavalry adder-service adder-client; do
        [ -x "$(program older "$built")" ] || fail "N's build left no $(program older "$built")"
    done
    say "built N in $(( $(date +%s) - at )) s, from target/upgrade/$short/src into target/upgrade/$short/target"
fi

step="2 (what doctor of N+1 must say of a member of each build)"
# What doctor of N+1 must say of a member of each build, at the end of the member's
# `note cluster-member` line, and what a step's line calls it (ADR 0137). A member
# stores its version in `cluster.protocol_version` when its tree has that counter, and
# doctor names it: version <n>, the number in the first line of the tree's page, `#
# Cluster protocol, version <n>`, which is 0.<n>.0 in semantic form, since the page's
# version is the minor while the major is 0. Of a member with no such counter doctor
# says that it runs a build older than the counter. Whether N's members have the
# counter is read from N's counters.toml alone, in N's exported tree, and not from
# the version of N's page: every tree whose page is version 0 lacks the counter, and
# so does the first tree of version 1, the one before ADR 0137.
page_version() { sed -n '1s/^# Cluster protocol, version \([0-9][0-9]*\)$/\1/p'; }
newer_version=$(page_version <spec/cluster.md)
[ -n "$newer_version" ] \
    || fail "the first line of spec/cluster.md is \`$(sed -n 1p spec/cluster.md)\`, and the script reads this tree's version from a line of the form \`# Cluster protocol, version <n>\`; when the page's title changes, teach page_version in conformance/upgrade/run.sh the new form"
newer_clause="it runs version $newer_version of the cluster protocol, 0.$newer_version.0"
newer_called="at version $newer_version"
if grep -q '^name = "cluster.protocol_version"$' "$older/src/counters.toml" 2>/dev/null; then
    older_version=""
    if [ -f "$older/src/spec/cluster.md" ]; then older_version=$(page_version <"$older/src/spec/cluster.md"); fi
    [ -n "$older_version" ] \
        || fail "N, $short, has cluster.protocol_version in its counters.toml, and its exported tree, target/upgrade/$short/src, has no spec/cluster.md whose first line is of the form \`# Cluster protocol, version <n>\`, so the script cannot say which version doctor must name N's members; name another N with CAVALRY_UPGRADE_FROM, or teach page_version in conformance/upgrade/run.sh the form of N's title"
    older_clause="it runs version $older_version of the cluster protocol, 0.$older_version.0"
    older_called="at version $older_version"
    say "N's counters.toml, in target/upgrade/$short/src, has cluster.protocol_version, so a member at N stores its version"
else
    older_clause="it has no cluster.protocol_version, so it runs a build older than that counter: version 0 of the cluster protocol, or the first build of version 1"
    older_called="with no cluster.protocol_version"
    say "N's counters.toml, in target/upgrade/$short/src, has no cluster.protocol_version, so a member at N stores no version"
fi
say "doctor of N+1 must say of a member at N+1 \`$newer_clause\` and of a member at N \`$older_clause\`"

# The three members of one cluster, each with five loopback ports, and one more port
# for the client's response channel, counted as conformance/c/run.sh counts its own:
# out of 10000 to 19999, from a hash of the shell's process id, never bound to be
# chosen.
first_port=$(( ($$ * 2654435761) % 10000 ))
for id in 0 1 2; do
    member="$id"
    for i in 0 1 2 3 4; do
        member="$member,127.0.0.1:$(( 10000 + (first_port + id * 5 + i) % 10000 ))"
    done
    members="${members:+$members|}$member"
done
response="127.0.0.1:$(( 10000 + (first_port + 15) % 10000 ))"

# Starts member `$1` from build `$2` on the member's two directories. Its output goes
# to a file of that start.
start_member() {
    CAVALRY_CLUSTER_MEMBERS=$members CAVALRY_CLUSTER_MEMBER_ID=$1 "$(program "$2" cavalry)" run \
        --dir "$root/member-$1" --log-dir "$root/member-$1-log" >"$root/member-$1.$2.out" 2>&1 &
    member_pid[$1]=$!
    pids="$pids $!"
    member_build[$1]=$2
}
# Waits for member `$1` to say that it started. Until it has, its driver directory may
# be the one its last run left, and doctor is not asked.
started() {
    await "$root/member-$1.${member_build[$1]}.out" "^cavalry run: cluster member $1 started" \
        "member $1, started at $(named "${member_build[$1]}"), to say \`cavalry run: cluster member $1 started\`" "${member_pid[$1]}"
}
# Starts the adder-service of member `$1`'s build beside the member.
start_service() {
    local build=${member_build[$1]}
    "$(program "$build" adder-service)" --dir "$root/member-$1" >"$root/service-$1.$build.out" 2>&1 &
    service_pid[$1]=$!
    pids="$pids $!"
    await "$root/service-$1.$build.out" "^adder-service member-$1: beside the driver at " \
        "the adder-service of $(named "$build") beside member $1 to say where it is" "${service_pid[$1]}"
}
# Stops member `$1` as an operator upgrades one: its service, then the member, each
# with SIGTERM.
stop_member() {
    local build=${member_build[$1]}
    stop "${service_pid[$1]}" "the service beside member $1"
    service_pid[$1]=""
    stop "${member_pid[$1]}" "member $1"
    member_pid[$1]=""
    grep -q '^cavalry run: closed; ' "$root/member-$1.$build.out" \
        || fail "member $1, at $(named "$build"), was stopped with SIGTERM and did not say \`cavalry run: closed\`"
}
# Starts the client's driver from build `$1`: a driver that is no member.
start_client_driver() {
    "$(program "$1" cavalry)" run --dir "$root/client" --log-dir "$root/client-log" >"$root/client-driver.$1.out" 2>&1 &
    client_driver_pid=$!
    pids="$pids $!"
    await "$root/client-driver.$1.out" "^cavalry run: driver started on " \
        "the client's driver, at $(named "$1"), to say that it started" "$client_driver_pid"
}

# One line for what doctor says of member `$1`: `leader <term> <append position>
# <commit position>`, or `follower <term> <append position> <commit position> <the
# member it follows>`, each with the election closed; nothing for any other state.
state() {
    doctor "$1" | sed -n \
        -e 's/.*this driver is cluster member [0-9]*, the leader in term \([0-9]*\), election closed: append position \([0-9]*\), commit position \([0-9]*\).*/leader \1 \2 \3/p' \
        -e 's/.*this driver is cluster member [0-9]*, a follower in term \([0-9]*\), election closed: append position \([0-9]*\), commit position \([0-9]*\); it follows member \([0-9]*\).*/follower \1 \2 \3 \4/p'
}
# Prints `<leader> <term> <position>` once the members `$1` are one cluster at rest: one
# of them says that it leads a term above `$2` with its election closed, every other
# says that it follows that member in that term with its election closed, and all of
# them report one position as append position and commit position. 60 s at most.
settled() {
    local deadline=$((SECONDS + 60)) id role term append commit follows agreed leaders one
    while [ "$SECONDS" -lt "$deadline" ]; do
        agreed=""
        leaders=0
        for id in $1; do
            read -r role term append commit follows <<<"$(state "$id")" || true
            if [ "$role" = leader ]; then
                follows=$id
                leaders=$((leaders + 1))
            elif [ "$role" != follower ]; then
                agreed=no
                break
            fi
            if [ "$append" != "$commit" ] || [ "$term" -le "$2" ]; then
                agreed=no
                break
            fi
            one="$follows $term $commit"
            if [ -z "$agreed" ]; then
                agreed=$one
            elif [ "$agreed" != "$one" ]; then
                agreed=no
                break
            fi
        done
        if [ "$agreed" != no ] && [ "$leaders" = 1 ]; then
            echo "$agreed"
            return 0
        fi
        sleep 0.1
    done
    return 1
}
# What doctor of N+1 named the members at the last rest, for a step's line.
versions=""
# Holds what doctor of N+1 names each member of `$1` to the build the script started
# the member from: the member's `note cluster-member` line must end with that build's
# clause, after a `; ` and before the `; see ` and the page that every finding ends
# with. Sets `versions`. A member whose election has closed added its counters before
# that, the version's among them, and the counter never moves, so nothing is waited
# for.
named_versions() {
    local id said note want at_newer="" at_older=""
    for id in $1; do
        said=$(doctor_newer "$id")
        note=$(sed -n 's/.*note cluster-member  *\(this driver is cluster member .*\)/\1/p' <<<"$said")
        if [ "${member_build[$id]}" = newer ]; then
            want=$newer_clause
            at_newer="${at_newer:+$at_newer }$id"
        else
            want=$older_clause
            at_older="${at_older:+$at_older }$id"
        fi
        case "$note" in
            *"; $want; see "*) ;;
            *) fail "doctor of N+1 does not say of member $id, which the script started at $(named "${member_build[$id]}"), what a member of that build holds in cluster.protocol_version: its \`note cluster-member\` line must end with \`; $want; see \` and the page that explains it, and the line is \`${note:-absent}\`; a member whose tree has the counter stores its version there when it starts, and doctor reads it there (docs/decisions/0137-hold-a-members-version-of-the-cluster-protocol-in-a-counter-and-no-peers.md); doctor of N+1 said: $said" ;;
        esac
    done
    versions=""
    if [ -n "$at_newer" ]; then versions="$(members_text "$at_newer") $newer_called"; fi
    if [ -n "$at_older" ]; then versions="${versions:+$versions and }$(members_text "$at_older") $older_called"; fi
    versions="doctor of N+1 names $versions"
}
# Waits for the members `$1` to be one cluster at rest, whose leader's term is above
# `$2`, and sets `leading`, `term` and `position`; `$3` is what the wait is for. Then
# holds what doctor of N+1 names each of them, and sets `versions`.
rest() {
    read -r leading term position < <(settled "$1" "$2") \
        || fail "waited 60 s for $3: doctor on members $1 must show one of them as the leader of a term above $2 with its election closed, every other as a follower of it in that term with its election closed, and one position as append position and commit position on all of them"
    named_versions "$1"
}
# Fails unless member `$1` still leads term `$2`; `$3` is what happened since.
same_lead() {
    [ "$leading" = "$1" ] && [ "$term" = "$2" ] \
        || fail "$3 moved the lead: member $1 led term $2 before it, and member $leading leads term $term after it; a member that starts again joins the leader the others elected (docs/how-to/run-a-cluster.md, step 7)"
}

# The client's state: the total the cluster must hold, how many times the client has
# run, and the session and the last number of its last run.
total=0
runs=0
session=""
last=""
# Runs the adder-client of build `$1` with the numbers after it. The leader that
# doctor names must open the session, and the cluster must answer each number with the
# total so far: the number is committed, applied and acknowledged.
add() {
    local build=$1 out number
    shift
    runs=$((runs + 1))
    out="$root/client-$runs.$build.out"
    "$(program "$build" adder-client)" --dir "$root/client" --members "$members" --response "$response" "$@" >"$out" 2>&1 \
        || fail "the adder-client of $(named "$build") failed with the numbers $*; it said: $(cat "$out")"
    session=$(sed -n 's/^adder-client: session \([0-9]*\) is open; .*/\1/p' "$out")
    [ -n "$session" ] || fail "the adder-client of $(named "$build") did not say which session it opened; it said: $(cat "$out")"
    grep -q "^adder-client: session $session is open; the leader is member $leading, in term $term\$" "$out" \
        || fail "the adder-client of $(named "$build") did not open its session with member $leading in term $term, the leader doctor names; it said: $(cat "$out")"
    for number in "$@"; do
        total=$((total + number))
        grep -q "^adder-client: sent $number; the cluster answered $total (term $term, cluster time [0-9]* ms)\$" "$out" \
            || fail "the cluster did not answer $number with the total $total in term $term; the adder-client of $(named "$build") said: $(cat "$out")"
        last=$number
    done
}
# Waits for the service beside each member of `$1` to have read the client's last run:
# its last number with the total, and its session's close, the run's last entry. With
# `$2`, the leader, the service beside it must have replied and no other. Without it
# the reply is not looked at: a service that started since reads entries that a service
# before it answered.
applied() {
    local id out reply
    for id in $1; do
        out="$root/service-$id.${member_build[$id]}.out"
        if [ -z "$2" ]; then
            reply=""
        elif [ "$id" = "$2" ]; then
            reply="this service replied\$"
        else
            reply="the reply is not this service's to send\$"
        fi
        await "$out" "^adder-service member-$id: position [0-9]*, term [0-9]*: session $session sent $last; the total is $total; $reply" \
            "the service beside member $id, at $(named "${member_build[$id]}"), to read session $session's $last with the total at $total${2:+, and the service beside member $2 alone to reply}" \
            "${service_pid[$id]}"
        await "$out" "^adder-service member-$id: position [0-9]*, term [0-9]*: session $session closed" \
            "the service beside member $id, at $(named "${member_build[$id]}"), to read the close of session $session" "${service_pid[$id]}"
    done
}

step="a (three members at N)"
if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
root=$(mktemp -d "${base%/}/cavalry-upgrade.XXXXXX")
upgrade_began=$(date +%s)
for id in 0 1 2; do start_member "$id" older; done
for id in 0 1 2; do started "$id"; done
rest "0 1 2" -1 "three members at N to elect a leader"
for id in 0 1 2; do start_service "$id"; done
start_client_driver older
add older 5 7
applied "0 1 2" "$leading"
was_leading=$leading
was_term=$term
rest "0 1 2" $((term - 1)) "three members at N to hold what the client added"
same_lead "$was_leading" "$was_term" "the client's run"
say "a. $(builds): member $leading leads term $term and the two others follow it, elections closed; N's adder-client added 5 and 7 and read $total; every service holds $total; commit position $position on all three; $versions"

step="b (upgrade a follower)"
upgraded=""
for id in 0 1 2; do
    if [ "$id" != "$leading" ]; then upgraded=$id; break; fi
done
stop_member "$upgraded"
start_member "$upgraded" newer
started "$upgraded"
start_service "$upgraded"
rest "0 1 2" $((term - 1)) "member $upgraded, started at N+1 on the log N wrote, to follow member $was_leading"
same_lead "$was_leading" "$was_term" "member $upgraded's start at N+1"
applied "$upgraded" ""
read_before=$total
add older 11 13
applied "0 1 2" "$leading"
rest "0 1 2" $((term - 1)) "two members at N and one at N+1 to hold what the client added"
same_lead "$was_leading" "$was_term" "the client's run"
say "b. $(builds): member $upgraded follows member $leading in term $term, election closed, and its service, at N+1, read the log N wrote to the total $read_before; N's adder-client added 11 and 13 and read $total; every service holds $total; commit position $position on all three; $versions"

step="c (stop the leader, at N)"
stopped=$leading
remaining=""
last_older=""
for id in 0 1 2; do
    [ "$id" = "$stopped" ] || remaining="${remaining:+$remaining }$id"
    if [ "$id" != "$stopped" ] && [ "$id" != "$upgraded" ]; then last_older=$id; fi
done
stop_member "$stopped"
rest "$remaining" "$was_term" "members ${remaining/ / and }, one at N and one at N+1, to elect a leader after member $stopped's stop"
was_leading=$leading
was_term=$term
stop "$client_driver_pid" "the client's driver"
start_client_driver newer
add newer 17 19
applied "$remaining" "$leading"
rest "$remaining" $((term - 1)) "one member at N and one at N+1 to hold what the client added"
same_lead "$was_leading" "$was_term" "the client's run"
say "c. $(builds): members ${remaining/ / and }, one at each build, elected member $leading, at $(named "${member_build[$leading]}"), in term $term; N+1's adder-client added 17 and 19 and read $total; both services hold $total; commit position $position on both; $versions"

step="c (start the stopped member at N+1)"
start_member "$stopped" newer
started "$stopped"
start_service "$stopped"
rest "0 1 2" $((term - 1)) "member $stopped, started at N+1, to follow member $was_leading"
same_lead "$was_leading" "$was_term" "member $stopped's start at N+1"
applied "$stopped" ""
say "c. $(builds): member $stopped follows member $leading in term $term, election closed, and its service read the log to $total; commit position $position on all three; $versions"

step="d (upgrade the last member at N)"
stop_member "$last_older"
start_member "$last_older" newer
started "$last_older"
start_service "$last_older"
if [ "$last_older" = "$was_leading" ]; then
    rest "0 1 2" "$was_term" "three members at N+1 to elect a leader after member $last_older, which led term $was_term, was upgraded"
    elected="it led term $was_term, so the three elected member $leading in term $term"
else
    rest "0 1 2" $((term - 1)) "member $last_older, started at N+1, to follow member $was_leading"
    same_lead "$was_leading" "$was_term" "member $last_older's start at N+1"
    elected="it follows member $leading in term $term"
fi
was_leading=$leading
was_term=$term
applied "$last_older" ""
add newer 23 29
applied "0 1 2" "$leading"
rest "0 1 2" $((term - 1)) "three members at N+1 to hold what the client added"
same_lead "$was_leading" "$was_term" "the client's run"
say "d. $(builds): member $last_older was upgraded, and $elected, elections closed; N+1's adder-client added 23 and 29 and read $total; every service holds $total; commit position $position on all three; $versions"

step="e (nothing committed is lost and the logs are one log)"
digest() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
# Each service's last total, and what each read: its lines without its member's name
# and without who replied, the two things that differ between services.
for id in 0 1 2; do
    out="$root/service-$id.newer.out"
    held=$(sed -n 's/.*; the total is \(-\{0,1\}[0-9]*\); .*/\1/p' "$out" | tail -n 1)
    [ "$held" = "$total" ] || fail "the service beside member $id ends with the total ${held:-none}, and the client's total is $total"
    grep '^adder-service member-[0-9]*: position ' "$out" | sed -e 's/^adder-service member-[0-9]*: //' \
        -e 's/; this service replied$//' -e "s/; the reply is not this service's to send\$//" >"$root/service-$id.read"
done
read_0=$(digest "$root/service-0.read")
read_1=$(digest "$root/service-1.read")
read_2=$(digest "$root/service-2.read")
[ "$read_0" = "$read_1" ] && [ "$read_1" = "$read_2" ] \
    || fail "the three services did not read one log: what each printed, without its member's name and who replied, hashes to $read_0, $read_1 and $read_2; compare service-0.read, service-1.read and service-2.read"
say "e. every member's service holds the client's total, $total, and the three read the same $(wc -l <"$root/service-0.read" | tr -d ' ') entries from position 0: $read_0"

# Each member's file of its log's recordings, copied with one open while the member
# runs: the member removes the file when it closes, and a driver that starts on the
# directory removes what is left (spec/cluster.md, "The log's recordings").
end=$position
for id in 0 1 2; do
    cat "$root/member-$id/log-recordings-$id" >"$root/log-recordings-$id" \
        || fail "member $id runs and its driver directory holds no file log-recordings-$id that can be read"
done
# The followers first, then the leader, so that no member begins an election.
for id in 0 1 2; do
    [ "$id" = "$leading" ] || stop_member "$id"
done
stop_member "$leading"
stop "$client_driver_pid" "the client's driver"

# The integer of `$3` bytes at offset `$2` of file `$1`, little-endian, read through od
# in hex as conformance/log-member.sh reads it.
field() {
    echo $((16#$(od -An -t "x$3" -j "$2" -N "$3" "$1" | tr -d ' \n')))
}
# The recording that holds position `$2` of the log that the copy `$1` of a member's
# file lists: the first entry that begins at or below the position and stops above
# it, and the live recording when no entry is so.
recording_at() {
    local count i=0 start stop_position
    count=$(field "$1" 32 4)
    while [ "$i" -lt "$count" ]; do
        start=$(field "$1" $((40 + i * 24 + 8)) 8)
        stop_position=$(field "$1" $((40 + i * 24 + 16)) 8)
        if [ "$start" -le "$2" ] && [ "$2" -lt "$stop_position" ]; then
            field "$1" $((40 + i * 24)) 8
            return 0
        fi
        i=$((i + 1))
    done
    field "$1" 16 8
}

messages=""
for id in 0 1 2; do
    list="$root/log-recordings-$id"
    # The reader is held to the file's own header before any field is trusted: the
    # magic, and the length that the entry count gives.
    [ "$(head -c 8 "$list")" = CVLYLREC ] \
        || fail "member $id's file log-recordings-$id does not begin with CVLYLREC, so it is no file of spec/cluster.md, \"The log's recordings\""
    entries=$(field "$list" 32 4) || fail "this host's od did not read the entry count of member $id's file log-recordings-$id"
    [ "$(wc -c <"$list" | tr -d ' ')" = $((40 + entries * 24)) ] \
        || fail "member $id's file log-recordings-$id holds $(wc -c <"$list" | tr -d ' ') bytes, and its entry count, read as $entries, gives $((40 + entries * 24)); the reader, the functions field and recording_at, reads little-endian integers through od"

    "$(program newer cavalry)" run --dir "$root/member-$id" --log-dir "$root/member-$id-log" >"$root/replay-driver-$id.out" 2>&1 &
    replay_driver=$!
    pids="$pids $replay_driver"
    await "$root/replay-driver-$id.out" "^cavalry run: driver started on " \
        "a driver at N+1 that is no member to start on member $id's directories" "$replay_driver"
    # The log from position 0, across the member's recordings: each is replayed from
    # where the one before ended, to its own end, and the outputs are joined.
    : >"$root/member-$id.body"
    at_position=0
    replayed=0
    parts=""
    part=0
    while [ "$at_position" -lt "$end" ]; do
        part=$((part + 1))
        [ "$part" -le 1026 ] || fail "member $id's log is in more recordings than its file can list"
        recording=$(recording_at "$list" "$at_position") \
            || fail "member $id's file log-recordings-$id could not be read for position $at_position"
        "$(program newer cavalry)" replay --dir "$root/member-$id" --log-dir "$root/member-$id-log" \
            --recording "$recording" --from "$at_position" >"$root/member-$id.part" 2>"$root/member-$id.part.err" &
        replay=$!
        pids="$pids $replay"
        ended "$replay" "the replay of member $id's recording $recording from position $at_position"
        wait "$replay" \
            || fail "\`cavalry replay --recording $recording --from $at_position\` failed on member $id's directories; it said: $(cat "$root/member-$id.part.err")"
        forget "$replay"
        summary=$(tail -n 1 "$root/member-$id.part")
        ended_at=$(sed -n 's/.*ended at position \([0-9]*\): [0-9]* messages from position [0-9]*.*/\1/p' <<<"$summary")
        count=$(sed -n 's/.*ended at position [0-9]*: \([0-9]*\) messages from position [0-9]*.*/\1/p' <<<"$summary")
        [ -n "$ended_at" ] && [ -n "$count" ] \
            || fail "the replay of member $id's recording $recording from position $at_position did not say where it ended; its last line: $summary"
        [ "$ended_at" -gt "$at_position" ] \
            || fail "the replay of member $id's recording $recording from position $at_position ended at $ended_at and played nothing, below the commit position $end: the member's log has a gap there"
        sed '$d' "$root/member-$id.part" >>"$root/member-$id.body"
        replayed=$((replayed + count))
        parts="${parts:+$parts, }$recording@$at_position"
        at_position=$ended_at
    done
    stop "$replay_driver" "the driver that replayed member $id's log"
    [ "$at_position" = "$end" ] \
        || fail "member $id's log replays to position $at_position, and the three members reported the commit position $end"
    [ "$replayed" -gt 0 ] || fail "member $id's log replays to no message"
    if [ -z "$messages" ]; then messages=$replayed; fi
    [ "$replayed" = "$messages" ] \
        || fail "member $id's log replays to $replayed messages, and member 0's to $messages"
    say "e. member $id's log replays to position $at_position, $replayed messages, $(digest "$root/member-$id.body"), through its recordings, each as <recording>@<the position it is replayed from>: $parts"
done
log_0=$(digest "$root/member-0.body")
log_1=$(digest "$root/member-1.body")
log_2=$(digest "$root/member-2.body")
[ "$log_0" = "$log_1" ] && [ "$log_1" = "$log_2" ] \
    || fail "the three members' logs are not one log: their replays hash to $log_0, $log_1 and $log_2; compare member-0.body, member-1.body and member-2.body"
say "e. the three members' logs replay to the same bytes: $messages messages to position $end, three equal hashes"

step="f (stop everything)"
for pid in $pids; do
    stop "$pid" "a program the script started"
done
finished=$(date +%s)
say "f. every program the script started has ended; the upgrade took $(( finished - upgrade_began )) s and the whole run $(( finished - began )) s"
say "ok"
