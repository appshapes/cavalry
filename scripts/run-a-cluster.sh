#!/usr/bin/env bash
# The cluster tutorial's path, end to end and timed (docs/tutorials/run-a-cluster.md;
# ADR 0052, feature 5; ADR 0126). `just quickstart` runs it after the README's path, so
# CI runs it on Linux and macOS. Every step is one the page tells a reader to type, with
# the page's number, and the script fails on the first step that does not print what
# the page says the reader will see.
#
# What differs from the page, and why:
#
# - The page's directory is /tmp/cavalry-tutorial and its ports are 9000 to 9024 and
#   9100, which a reader can type. The script's directory is one of its own, in /dev/shm
#   when it exists and the temporary directory otherwise, and its sixteen ports are
#   counted out of 10000 to 19999 from the shell's process id, as
#   conformance/python/run.sh counts its own, so that a cluster a reader left running
#   on this host is left alone. The page's own members string is checked with
#   `cavalry doctor`, as docs/how-to/run-a-cluster.md's step 2 checks one.
# - The page's reader has two terminals, and the members string is exported in the
#   first alone. The script exports it once and takes it out for the client's driver,
#   which the reader starts in the second terminal.
# - The page stops a program by its place among the terminal's background programs,
#   `kill '%?member-2'`. The script stops it by its process id, with the same signal.
# - Each program's output goes to a file under the script's directory, and the script
#   prints it when it checks the step.
#
# Every wait is bounded at 60 s.
set -euo pipefail

cd "$(dirname "$0")/.."
started=$(date +%s)

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
root="${base%/}/cavalry-run-a-cluster-$$"
rm -rf "$root"
mkdir -p "$root"
pids=""
cleanup() {
    for pid in $pids; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    rm -rf "$root"
}
trap cleanup EXIT

step() { printf '\nrun-a-cluster: %s\n' "$1"; }
# Fails with what was expected, and prints what every program said.
fail() {
    echo "run-a-cluster: $1; see docs/tutorials/run-a-cluster.md" >&2
    for out in "$root"/*.out; do
        [ -f "$out" ] || continue
        echo "--- $(basename "$out")" >&2
        cat "$out" >&2
    done
    exit 1
}
# Waits until file `$1` holds a line that matches `$2`, 60 s at most; `$3` is what to
# say when it does not.
await() {
    SECONDS=0
    until grep -q -- "$2" "$1" 2>/dev/null; do
        [ "$SECONDS" -lt 60 ] || fail "$3"
        sleep 0.1
    done
}

step "1. build the binary and the two programs"
cargo build --release -p cavalry --bin cavalry --example adder-service --example adder-client
cavalry=target/release/cavalry
service=target/release/examples/adder-service
client=target/release/examples/adder-client

# The page's members string as written, held to the check the how-to makes of one
# before anything starts.
written=$(sed -n "s/^export CAVALRY_CLUSTER_MEMBERS='\(.*\)'$/\1/p" docs/tutorials/run-a-cluster.md)
[ -n "$written" ] || fail "docs/tutorials/run-a-cluster.md has no line that exports CAVALRY_CLUSTER_MEMBERS"
checked=$(CAVALRY_CLUSTER_MEMBERS="$written" CAVALRY_CLUSTER_MEMBER_ID=0 "$cavalry" doctor --dir "$root/member-0" || true)
grep -q 'FAIL driver-directory' <<<"$checked" || fail "doctor did not say FAIL driver-directory before a driver ran"
if grep -q 'FAIL cluster-member' <<<"$checked"; then
    printf '%s\n' "$checked" >&2
    fail "doctor refused the members string the page gives"
fi

# The script's own string: the same shape, on ports counted from the process id.
first=$(( ($$ * 2654435761) % 10000 ))
CAVALRY_CLUSTER_MEMBERS=""
for id in 0 1 2; do
    member="$id"
    for i in 0 1 2 3 4; do
        member="$member,127.0.0.1:$(( 10000 + (first + id * 5 + i) % 10000 ))"
    done
    CAVALRY_CLUSTER_MEMBERS="${CAVALRY_CLUSTER_MEMBERS:+$CAVALRY_CLUSTER_MEMBERS|}$member"
done
export CAVALRY_CLUSTER_MEMBERS
response="127.0.0.1:$(( 10000 + (first + 15) % 10000 ))"

# Starts member `$1`, with its output in `$2`, and waits for its two lines.
member_pid=()
start_member() {
    CAVALRY_CLUSTER_MEMBER_ID=$1 "$cavalry" run --dir "$root/member-$1" --log-dir "$root/member-$1-log" \
        >"$2" 2>&1 &
    member_pid[$1]=$!
    pids="$pids $!"
}
# What `cavalry doctor` says of member `$1`: its `cluster-member` note.
doctor() {
    CAVALRY_CLUSTER_MEMBER_ID=$1 "$cavalry" doctor --dir "$root/member-$1" --log-dir "$root/member-$1-log" 2>/dev/null \
        | grep 'cluster member' || true
}
# Prints "<member id> <term>" for the member among `$2` that says it leads a term above
# `$1` with its election closed: 60 s at most.
leader() {
    SECONDS=0
    while [ "$SECONDS" -lt 60 ]; do
        for id in $2; do
            term=$(doctor "$id" | sed -n 's/.*the leader in term \([0-9]*\), election closed.*/\1/p')
            if [ -n "$term" ] && [ "$term" -gt "$1" ]; then
                echo "$id $term"
                return 0
            fi
        done
        sleep 0.1
    done
    return 1
}
# The line the service beside member `$1` prints for a reply: its own to send when its
# member `$2` leads.
replied() {
    if [ "$1" = "$2" ]; then echo "this service replied"; else echo "the reply is not this service's to send"; fi
}

step "2. start three members: $CAVALRY_CLUSTER_MEMBERS"
for id in 0 1 2; do
    start_member "$id" "$root/member-$id.out"
done
for id in 0 1 2; do
    await "$root/member-$id.out" "^cavalry run: driver started on $root/member-$id (pid [0-9]*), recording into $root/member-$id-log, conductor idle strategy backoff; Ctrl-C or SIGTERM stops it$" \
        "member $id did not say that its driver started"
    await "$root/member-$id.out" "^cavalry run: cluster member $id started" "member $id did not say that it started"
done
cat "$root"/member-?.out

step "3. see which member leads"
read -r leading term < <(leader -1 "0 1 2") || fail "no member said that it leads within 60 s"
for id in 0 1 2; do
    said=$(doctor "$id")
    printf '%s\n' "$said"
    if [ "$id" = "$leading" ]; then
        grep -q "this driver is cluster member $id, the leader in term $term, election closed: " <<<"$said" \
            || fail "member $id did not say that it leads term $term"
    else
        # A follower names its leader once it has heard the term's first entry.
        SECONDS=0
        until grep -q "this driver is cluster member $id, a follower in term $term, election closed: .*; it follows member $leading; " <<<"$said"; do
            [ "$SECONDS" -lt 60 ] || fail "member $id did not say that it follows member $leading in term $term"
            sleep 0.1
            said=$(doctor "$id")
        done
    fi
done

step "4. start a service beside each member"
for id in 0 1 2; do
    "$service" --dir "$root/member-$id" >"$root/service-$id.out" 2>&1 &
    pids="$pids $!"
done
for id in 0 1 2; do
    await "$root/service-$id.out" "^adder-service member-$id: beside the driver at $root/member-$id, reading its member's committed log$" \
        "the service beside member $id did not say where it is"
    await "$root/service-$id.out" "^adder-service member-$id: position 0: term $term began, led by member $leading$" \
        "the service beside member $id did not read the term's first entry"
done
cat "$root"/service-?.out

step "5. start a driver for the client, where the members string is not set"
env -u CAVALRY_CLUSTER_MEMBERS "$cavalry" run --dir "$root/client" --log-dir "$root/client-log" \
    >"$root/client-driver.out" 2>&1 &
pids="$pids $!"
await "$root/client-driver.out" "^cavalry run: driver started on $root/client (pid [0-9]*), recording into $root/client-log, conductor idle strategy backoff; Ctrl-C or SIGTERM stops it$" \
    "the client's driver did not say that it started"
cat "$root/client-driver.out"
# What the page says of a driver started where the string is set: it does not start,
# and says so. In the background, so that one that did start is a bounded wait here.
"$cavalry" run --dir "$root/refused" --log-dir "$root/refused-log" >"$root/refused.out" 2>&1 &
refused=$!
pids="$pids $refused"
await "$root/refused.out" "CAVALRY_CLUSTER_MEMBERS is set and CAVALRY_CLUSTER_MEMBER_ID is unset" \
    "a cavalry run with the members string and no member id did not say why it did not start"
if wait "$refused"; then
    fail "a cavalry run with the members string and no member id exited 0"
fi

step "6. send the cluster two numbers"
"$client" --dir "$root/client" --response "$response" 5 7 >"$root/client-1.out" 2>&1 || fail "adder-client failed"
cat "$root/client-1.out"
[ "$(wc -l <"$root/client-1.out" | tr -d ' ')" = 4 ] || fail "the client did not print four lines"
grep -q "^adder-client: session 0 is open; the leader is member $leading, in term $term$" "$root/client-1.out" \
    || fail "the client did not say that member $leading opened session 0 in term $term"
grep -q "^adder-client: sent 5; the cluster answered 5 (term $term, cluster time [0-9]* ms)$" "$root/client-1.out" \
    || fail "the client did not read the total 5"
grep -q "^adder-client: sent 7; the cluster answered 12 (term $term, cluster time [0-9]* ms)$" "$root/client-1.out" \
    || fail "the client did not read the total 12"
grep -q "^adder-client: session 0 is closed$" "$root/client-1.out" || fail "the client did not close its session"
# Every service applied both messages at the same positions and holds the same total,
# and one replied: the leader's.
for id in 0 1 2; do
    await "$root/service-$id.out" "^adder-service member-$id: position 96, term $term: session 0 opened$" \
        "the service beside member $id did not read the session's open"
    await "$root/service-$id.out" "^adder-service member-$id: position 224, term $term: session 0 sent 5; the total is 5; $(replied "$id" "$leading")$" \
        "the service beside member $id did not add 5 at position 224, or the wrong service replied"
    await "$root/service-$id.out" "^adder-service member-$id: position 320, term $term: session 0 sent 7; the total is 12; $(replied "$id" "$leading")$" \
        "the service beside member $id did not add 7 at position 320, or the wrong service replied"
    await "$root/service-$id.out" "^adder-service member-$id: position 416, term $term: session 0 closed$" \
        "the service beside member $id did not read the session's close"
done
grep -h "session 0" "$root"/service-?.out

step "7. stop the leader, member $leading"
kill -TERM "${member_pid[$leading]}"
wait "${member_pid[$leading]}" || true
grep -q "^cavalry run: stopping$" "$root/member-$leading.out" || fail "member $leading did not say that it was stopping"
grep -q "^cavalry run: closed; $root/member-$leading stays for doctor, and the next driver remakes it$" "$root/member-$leading.out" \
    || fail "member $leading did not say that it closed"
tail -n 2 "$root/member-$leading.out"
await "$root/service-$leading.out" "^adder-service: the driver at $root/member-$leading closed this service's client (driver-closed) with the total at 12" \
    "the service beside member $leading did not end with the total at 12"
tail -n 1 "$root/service-$leading.out"
others=""
for id in 0 1 2; do
    [ "$id" = "$leading" ] || others="${others:+$others }$id"
done
read -r next next_term < <(leader "$term" "$others") || fail "no member took the lead within 60 s of the leader's stop"
for id in $others; do
    await "$root/service-$id.out" "^adder-service member-$id: position 512: term $next_term began, led by member $next$" \
        "the service beside member $id did not read the first entry of term $next_term"
done
grep -h "term $next_term began" "$root"/service-?.out

step "8. send a number to the cluster that is left"
"$client" --dir "$root/client" --response "$response" 30 >"$root/client-2.out" 2>&1 \
    || fail "adder-client failed after the leader's stop"
cat "$root/client-2.out"
[ "$(wc -l <"$root/client-2.out" | tr -d ' ')" = 3 ] || fail "the client did not print three lines"
grep -q "^adder-client: session 1 is open; the leader is member $next, in term $next_term$" "$root/client-2.out" \
    || fail "the client did not say that member $next opened session 1 in term $next_term"
grep -q "^adder-client: sent 30; the cluster answered 42 (term $next_term, cluster time [0-9]* ms)$" "$root/client-2.out" \
    || fail "the client did not read the total 42 from the next leader's service"
for id in $others; do
    await "$root/service-$id.out" "^adder-service member-$id: position 736, term $next_term: session 1 sent 30; the total is 42; $(replied "$id" "$next")$" \
        "the service beside member $id did not add 30 at position 736, or the wrong service replied"
done
grep -h "session 1 sent" "$root"/service-?.out

step "9. start the stopped member again, then its service, and read the total"
start_member "$leading" "$root/member-$leading-again.out"
await "$root/member-$leading-again.out" "^cavalry run: cluster member $leading started" \
    "member $leading did not say that it started again"
"$service" --dir "$root/member-$leading" >"$root/service-$leading-again.out" 2>&1 &
pids="$pids $!"
# The whole log, read from its start: ten lines, with no reply of this service's.
await "$root/service-$leading-again.out" "^adder-service member-$leading: position 832, term $next_term: session 1 closed$" \
    "the service beside member $leading did not read the log to the close of session 1"
cat "$root/service-$leading-again.out"
[ "$(wc -l <"$root/service-$leading-again.out" | tr -d ' ')" = 10 ] || fail "the service that started again did not print ten lines"
for line in \
    "position 0: term $term began, led by member $leading" \
    "position 96, term $term: session 0 opened" \
    "position 224, term $term: session 0 sent 5; the total is 5; the reply is not this service's to send" \
    "position 320, term $term: session 0 sent 7; the total is 12; the reply is not this service's to send" \
    "position 416, term $term: session 0 closed" \
    "position 512: term $next_term began, led by member $next" \
    "position 608, term $next_term: session 1 opened" \
    "position 736, term $next_term: session 1 sent 30; the total is 42; the reply is not this service's to send"; do
    grep -q "^adder-service member-$leading: $line$" "$root/service-$leading-again.out" \
        || fail "the service that started again did not print: $line"
done
"$client" --dir "$root/client" --response "$response" 0 >"$root/client-3.out" 2>&1 \
    || fail "adder-client failed after the member started again"
cat "$root/client-3.out"
grep -q "^adder-client: sent 0; the cluster answered 42 (term $next_term, cluster time [0-9]* ms)$" "$root/client-3.out" \
    || fail "the client did not read the total 42"
for id in 0 1 2; do
    if [ "$id" = "$leading" ]; then out="$root/service-$id-again.out"; else out="$root/service-$id.out"; fi
    await "$out" "^adder-service member-$id: position [0-9]*, term $next_term: session 2 sent 0; the total is 42; $(replied "$id" "$next")$" \
        "the service beside member $id did not read the 0 of session 2 with the total at 42"
done
said=$(doctor "$leading")
printf '%s\n' "$said"
grep -q "this driver is cluster member $leading, a follower in term $next_term, election closed: .*; it follows member $next; " <<<"$said" \
    || fail "member $leading did not say that it follows member $next in term $next_term"

step "10. stop everything"
for pid in $pids; do
    if kill -0 "$pid" 2>/dev/null; then
        kill -TERM "$pid"
        wait "$pid" 2>/dev/null || true
    fi
done
pids=""
for out in "$root"/member-[012].out "$root/member-$leading-again.out" "$root/client-driver.out"; do
    # The leader's first run closed at step 7; every other driver closes here.
    grep -q "^cavalry run: closed; " "$out" || fail "$(basename "$out" .out) did not say that it closed"
done
if "$cavalry" doctor --dir "$root/client" >/dev/null 2>&1; then
    fail "doctor should exit 1 on a closed driver"
fi

printf '\nrun-a-cluster: the cluster tutorial took %s s\n' "$(( $(date +%s) - started ))"
