#!/usr/bin/env bash
# The log directory's upgrade gate (work item 362; ADR 0138, condition 9; ADR 0141;
# CLAUDE.md, invariants 1 and 3). A log directory written by an older build of
# Cavalry, N, opens under this tree, N+1, in place: with no step by an operator, with
# every record that N answered read back at its position, with what N purged still
# gone, and able to take a new recording. The rule it holds for every change of the
# durable log's format: a driver opens a directory of the format before it in place.
#
# The directory is the one Brigade's server keeps: the feed of
# docs/explanation/what-brigades-server-relies-on.md, one recorded publication whose
# every run is a new recording. Two servers hold such a directory in production. Each
# was written by the tree at 620a2e29 under Brigade's server 0.9.0, and since
# 2026-11-24 by the tree at d265af82 under server 0.10.0, which each started in place
# on it. The server reads its directory back at every start.
#
# The builds
#
# N+1 is this tree as it stands, built into target/release. The older builds are the
# trees that servers in production have run, each a commit (ADR 0141, ADR 0156):
#
# - 620a2e29e04751c7a1f930132666f85b3dd58e7a, the pin of Brigade's server 0.9.0, its tag
#   `server-v0.9.0` of github.com/appshapes/brigade
#   (`server/crates/brigade-log/Cargo.toml`). The servers ran it until 2026-11-24, and
#   each keeps a copy of its directory taken under it, as its way back.
# - d265af823b8311ed878ae1cc3643d52a58737cdc, the pin of server 0.10.0, its tag
#   `server-v0.10.0`, which both servers run since 2026-11-24.
#
# On CI's macOS runner, and there alone, the script builds 620a2e29 alone and writes
# server 0.9.0's directory alone, as it did before work item 441: the comment above
# "The older builds" below says why.
#
# CAVALRY_DIRECTORY_FROM, when it is set, names one commit, a hash, a tag or a branch,
# which takes the place of both: one older build, one directory that it writes alone,
# in the shape CAVALRY_DIRECTORY_SHAPE names, `pinned` when it is not set or
# `moved-to`, the shape server 0.10.0 writes.
#
# Each older tree is exported with `git archive` to target/upgrade/<its first 8 hex
# digits>/src and built into target/upgrade/<the same>/target, as
# conformance/upgrade/run.sh exports and builds its own N. Each build is the binary
# and one example: `cavalry` and `feed-directory`. 620a2e29 has no such example, and
# d265af82 has an older copy of it and of crates/cavalry/tests/feed/. The script puts
# this tree's crates/cavalry/examples/feed-directory.rs and crates/cavalry/tests/feed/
# in place in each exported tree, over the older copy where there is one, and builds
# them there: the feed's files use the calls of 620a2e29 and no later one, as the
# server's own file does in its releases 0.9.0 and 0.10.0. So this script also fails,
# at an older build, when those files stop compiling against that tree; step 2's
# message says what to do then.
#
# The directories
#
# Three directories, each that a server holds or may hold (ADR 0156). Each is written
# with its last run stopped, and again with it killed; the one that two builds write in
# turn a third time, with the first build's last run killed and the second's stopped,
# so that the second recovers a recording the first left live:
#
# - 0.9.0: written by 620a2e29 alone, in server 0.9.0's shape. The copy each server
#   keeps as its way back.
# - 0.9.0-then-0.10.0: written by 620a2e29 in server 0.9.0's shape and stopped, as
#   each server stopped on 2026-11-24, then opened in place by d265af82, which reads
#   back what 620a2e29 left and writes in server 0.10.0's shape. What each server holds
#   now.
# - 0.10.0: written by d265af82 alone, in server 0.10.0's shape. What a server first
#   installed at 0.10.0 holds.
#
# The shapes are the feed's two of crates/cavalry/tests/feed/process.rs: server
# 0.9.0's, `pinned`, in terms of 16 MiB, and server 0.10.0's, `moved-to`, `ssc=true`
# with `term-length=1m`.
#
# The schedule for each directory
#
# a. The older builds write the directory in turn. The first appends six records in
#    one run and stops; in a second run it appends five, releases what a snapshot at
#    the first run's end covers, appends five more, releases again and appends four.
#    A build that opens a directory another wrote begins where it ended, reads back
#    every record it left, appends five, releases the oldest recording left, and
#    appends four. Each release rolls the feed and purges the oldest recording, which
#    the script checks against what the feed says it let go of. The last run is
#    stopped as the server's clean stop does, or killed with SIGKILL, which leaves its
#    last recording live in the catalog. The script keeps what each run and each roll
#    recorded, and which recordings are left: steps b and c are held to that.
# b. `cavalry run` of N+1 starts on the directory as the older builds left it.
#    `cavalry replay` of N+1 plays each recording that is left, from its start to the
#    position its writer answered.
# c. N+1's feed, in the pinned shape, begins at the last position answered. It read
#    the catalog before its driver started, as server 0.10.0 does to size its room,
#    and found the longest term among the recordings left. It reads back every record
#    appended from the oldest recording kept, each at its position, is refused a read
#    from before that, and appends five records.
# d. N+1's feed, in the moved-to shape, begins where c ended, reads back what the older
#    builds wrote and c's records, and appends five records.
# e. N+1's feed begins where d ended and reads back all of it: recordings that two or
#    three builds wrote, in terms of two lengths.
#
# What a record read back is held to is the line the feed's process wrote when it
# appended the record: its length, a digest of its bytes and the position after it
# (crates/cavalry/tests/feed/process.rs).
#
# By hand
#
#   conformance/upgrade/log-directory.sh                                  the three directories
#   CAVALRY_DIRECTORY_FROM=<commit> conformance/upgrade/log-directory.sh  one, by the commit named
#   CAVALRY_DIRECTORY_FROM=d265af82 CAVALRY_DIRECTORY_SHAPE=moved-to conformance/upgrade/log-directory.sh
#   CAVALRY_DIRECTORY_FROM=HEAD conformance/upgrade/log-directory.sh      this tree's changes over its last commit
#
# The script's directories are under one made by `mktemp` in the temporary directory,
# on a disk, as a server's log directory is. Its two ports are counted out of 10000 to
# 19999 from the shell's process id, as conformance/c/run.sh counts its own. Every
# wait is bounded at 60 s and says what it waited for. On a failure the script prints
# the step and the reason and each program's output; it stops what it started, by
# process id, leaves its directories and says where, and exits 1. It runs under bash
# 3.2, which macOS has, and under bash 5.
set -euo pipefail

cd "$(dirname "$0")/../.."
repo=$(pwd)
began=$(date +%s)
# The script's programs take their directories by flag, whatever the caller's
# environment holds.
unset CAVALRY_DIR CAVALRY_CLUSTER_MEMBERS CAVALRY_CLUSTER_MEMBER_ID

# The trees that servers in production have run: server 0.9.0's pin and server 0.10.0's.
zero_nine=620a2e29e04751c7a1f930132666f85b3dd58e7a
zero_ten=d265af823b8311ed878ae1cc3643d52a58737cdc
step="1 (name the older builds)"
root=""
# The processes the script started that have not ended, by process id.
pids=""
reported=""
feed_pid=""

say() { printf 'conformance/upgrade/log-directory: %s\n' "$1"; }
# Fails the step with why. Called from the script's own shell alone, never from a
# function whose output is captured: an exit there would end the capture and not the
# script.
fail() {
    printf 'conformance/upgrade/log-directory: step %s failed: %s\n' "$step" "$1" >&2
    reported=yes
    exit 1
}
# The path of program `$2`, `cavalry` or `feed-directory`, in build `$1`: `newer`, this
# tree, or an older tree by its first 8 hex digits.
program() {
    local release
    if [ "$1" = newer ]; then release="$repo/target/release"; else release="$repo/target/upgrade/$1/target/release"; fi
    if [ "$2" = cavalry ]; then echo "$release/cavalry"; else echo "$release/examples/$2"; fi
}
# What a build is called in what the script prints.
named() { if [ "$1" = newer ]; then echo "N+1"; else echo "$1"; fi; }
# Takes process `$1` out of the list of those that run.
forget() {
    local pid kept=""
    for pid in $pids; do
        [ "$pid" = "$1" ] || kept="$kept $pid"
    done
    pids=$kept
}

# At any exit. After a failure: the step, when nothing said it yet, and every
# program's output. Always: every process the script started and that still runs is
# ended, by its process id, with SIGTERM and, 10 s later, SIGKILL. The directory goes
# after a clean run and stays after a failure.
finish() {
    local status=$? pid out deadline
    trap - EXIT
    set +e
    # The feed's input, when it is open. Closing a descriptor that is not open is no
    # error, and nothing else is redirected here: what follows prints to stderr.
    exec 3>&-
    if [ "$status" != 0 ]; then
        [ -n "$reported" ] || printf 'conformance/upgrade/log-directory: step %s failed: a command of the script exited %s, and its own message is above\n' "$step" "$status" >&2
        if [ -n "$root" ]; then
            for out in "$root"/*/*.out; do
                [ -f "$out" ] || continue
                printf -- '--- %s\n' "${out#"$root"/}" >&2
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
            printf 'conformance/upgrade/log-directory: the directories and every program'\''s output are left in %s; remove it when you have read them\n' "$root" >&2
        fi
    fi
    [ "$status" = 0 ] || exit 1
}
trap finish EXIT
trap 'reported=yes; printf "conformance/upgrade/log-directory: step %s was interrupted\n" "$step" >&2; exit 1' HUP INT TERM

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

# Sets `full` to the full hash of commit `$1`, which `$2` describes, or fails with how
# to fetch it. It sets a variable and prints nothing, since `fail` must not run where
# its output is captured.
resolve() {
    if full=$(git rev-parse --verify --quiet "$1^{commit}"); then
        return 0
    elif [ "$(git rev-parse --is-shallow-repository)" = true ]; then
        fail "this repository's history is shallow and does not hold $1, $2; fetch the whole history, \`git fetch --unshallow\`, or fetch-depth: 0 in the workflow's actions/checkout, and run this again"
    else
        fail "this repository does not hold $1, $2 and a commit of master's history; fetch master, \`git fetch origin master\`, and run this again"
    fi
}

# On CI's macOS runner, when no CAVALRY_DIRECTORY_FROM is set, the gate builds 620a2e29
# alone and writes server 0.9.0's directory alone, stopped and killed, as it did before
# work item 441. With the three directories written all seven ways, that runner lost
# communication with GitHub partway through `just conformance` in 2 of its first 4
# jobs, and in none of the 60 runs before; the job then keeps no log and skips its
# later steps. Linux, which the servers in production run, writes all seven. ADR 0160
# says why the exception stays, and work item 456 tracks the lost runner itself.
on_macos_ci=
if [ -z "${CAVALRY_DIRECTORY_FROM:-}" ] && [ "${GITHUB_ACTIONS:-}" = true ] && [ "${RUNNER_OS:-}" = macOS ]; then
    on_macos_ci=1
fi

# The older builds, each a full hash, and the directories, each `<name>=<writer>,...`
# with a writer `<build's first 8 hex digits>:<shape>`, written in that order.
if [ -n "${CAVALRY_DIRECTORY_FROM:-}" ]; then
    from=$(git rev-parse --verify --quiet "$CAVALRY_DIRECTORY_FROM^{commit}") \
        || fail "CAVALRY_DIRECTORY_FROM is \`$CAVALRY_DIRECTORY_FROM\`, which names no commit of this repository; give a hash, a tag or a branch that \`git rev-parse\` resolves, or unset it to take the trees the servers in production have run"
    named_shape=${CAVALRY_DIRECTORY_SHAPE:-pinned}
    case "$named_shape" in
        pinned | moved-to) ;;
        *) fail "CAVALRY_DIRECTORY_SHAPE is \`$named_shape\`; it takes \`pinned\`, server 0.9.0's shape, or \`moved-to\`, server 0.10.0's" ;;
    esac
    trees=$from
    directories="${from:0:8}=${from:0:8}:$named_shape"
    say "the older build is ${from:0:8}, named by CAVALRY_DIRECTORY_FROM: $(git log -1 --format=%s "$from")"
elif [ -n "$on_macos_ci" ]; then
    resolve "$zero_nine" "the commit Brigade's server 0.9.0 pins"
    first=$full
    trees=$first
    directories="0.9.0=${first:0:8}:pinned"
    say "the older build is ${first:0:8} alone, which Brigade's server 0.9.0 pins: on CI's macOS runner the gate writes server 0.9.0's directory alone, because with all seven that runner was lost partway through \`just conformance\` (work item 441)"
else
    resolve "$zero_nine" "the commit Brigade's server 0.9.0 pins"
    first=$full
    resolve "$zero_ten" "the commit Brigade's server 0.10.0 pins"
    second=$full
    trees="$first $second"
    directories="0.9.0=${first:0:8}:pinned 0.9.0-then-0.10.0=${first:0:8}:pinned,${second:0:8}:moved-to 0.10.0=${second:0:8}:moved-to"
    say "the older builds are ${first:0:8}, which Brigade's server 0.9.0 pins, and ${second:0:8}, which server 0.10.0 pins and the servers in production run"
fi
for tree in $trees; do
    say "${tree:0:8}'s spec/durable-log.md begins \`$(git show "$tree:spec/durable-log.md" 2>/dev/null | sed -n 1p)\`"
done
say "N+1 is this tree, whose page begins \`$(sed -n 1p spec/durable-log.md)\`"

step="2 (build N+1, this tree)"
at=$(date +%s)
cargo build --release -p cavalry --bin cavalry --example feed-directory
say "built N+1 in $(( $(date +%s) - at )) s, into target/release"

for tree in $trees; do
    short=${tree:0:8}
    older="$repo/target/upgrade/$short"
    step="2 (build $short)"
    at=$(date +%s)
    # Exported beside its place and moved there whole, so that an export that was cut
    # short is never taken for the tree: the directory is there only once tar has ended.
    if [ ! -d "$older/src" ]; then
        rm -rf "$older/src.partial"
        mkdir -p "$older/src.partial"
        git archive --format=tar "$tree" | tar -x -f - -C "$older/src.partial"
        mv "$older/src.partial" "$older/src"
    fi
    # The feed's files, from this tree into the older one: copied when they differ, so
    # that an unchanged file keeps its time and cargo builds nothing again.
    mkdir -p "$older/src/crates/cavalry/examples" "$older/src/crates/cavalry/tests/feed"
    for file in examples/feed-directory.rs tests/feed/mod.rs tests/feed/process.rs; do
        cmp -s "crates/cavalry/$file" "$older/src/crates/cavalry/$file" \
            || cp "crates/cavalry/$file" "$older/src/crates/cavalry/$file"
    done
    (cd "$older/src" && cargo build --release -p cavalry --bin cavalry --example feed-directory \
        --target-dir "$older/target") \
        || fail "$short did not build from target/upgrade/$short/src with this tree's feed in it; cargo's own message is above. When it names crates/cavalry/tests/feed/ or crates/cavalry/examples/feed-directory.rs, those files now use a call that $short does not have: the server pinned to $short cannot make that call either, so read docs/how-to/change-a-call-that-brigades-server-makes.md, \"When it does not work\", which says how this gate keeps a copy of the feed as that tree compiles it (ADR 0141)"
    for built in cavalry feed-directory; do
        [ -x "$(program "$short" "$built")" ] || fail "$short's build left no $(program "$short" "$built")"
    done
    say "built $short in $(( $(date +%s) - at )) s, from target/upgrade/$short/src into target/upgrade/$short/target, with this tree's feed-directory.rs and tests/feed/ in it"
done

# The recordings of the directory being written, as the script keeps them: each run
# records into a recording of its own, the next id of the catalog, and each release
# rolls the feed into the next. `kept` holds the recordings left, each as
# `<id>,<from>,<to>,<term length>`, oldest first; `cur_*` is the run's recording.
kept=""
next_id=0
cur_id=0
cur_from=0
cur_term=0
# The length of a term in shape `$1`.
term_of() { if [ "$1" = pinned ]; then echo 16777216; else echo 1048576; fi; }
# Field `$2` of recording `$1`: 1 the id, 2 where it begins, 3 where it ends, 4 its term.
field() { echo "$1" | cut -d, -f"$2"; }
# A run in shape `$1` began at `position`: its recording is the catalog's next.
began() {
    cur_id=$next_id
    next_id=$((next_id + 1))
    cur_from=$position
    cur_term=$(term_of "$1")
}
# The run's recording ended at `position`, its run stopped or killed: it is kept.
run_ended() { kept="${kept:+$kept }$cur_id,$cur_from,$position,$cur_term"; }
# Releases what precedes the end of the oldest recording kept, which purges it: the
# feed rolls at `position` into the next recording, and the release lets go of every
# recording that ends at or before that position. Fails unless the feed says it let go
# of as many as the script counts; `$1` names the build.
release_oldest() {
    local at entry left="" purged=0
    at=$(field "${kept%% *}" 3)
    ask "release $at" released
    run_ended
    cur_id=$next_id
    next_id=$((next_id + 1))
    cur_from=$position
    for entry in $kept; do
        if [ "$(field "$entry" 3)" -le "$at" ]; then
            purged=$((purged + 1))
        else
            left="${left:+$left }$entry"
        fi
    done
    kept=$left
    lines note | grep -q "^note let go of $purged recordings of the feed" \
        || fail "$1's release of position $at should have let go of $purged recordings of the feed, as the script counts them; the feed said: $(lines note)"
}
# The records are numbered from 0 across all of a directory's runs.
next_record=0
# Appends `$1` records, numbered on from the last.
append_records() {
    append "$next_record" "$1"
    next_record=$((next_record + $1))
}

# The feed's two addresses, counted as conformance/c/run.sh counts its own: out of
# 10000 to 19999, from a hash of the shell's process id, never bound to be chosen.
first_port=$(( ($$ * 2654435761) % 10000 ))
control="127.0.0.1:$(( 10000 + (first_port + 40) % 10000 ))"
own="127.0.0.1:$(( 10000 + (first_port + 41) % 10000 ))"

# The answers of a feed process: the lines that end a command's answer, one for each
# command of crates/cavalry/tests/feed/process.rs. An extended regular expression, which
# `counted`, `answered` and `lines` all take from here.
answers='^(open|appended|read|released|held|ended|failed|padded|synced|longest)( |$)'
# The feed process the script talks to: its output file, how many answers the script
# has read from it, and the last one.
feed_out=""
feed_answers=0
answer=""
# Starts the feed of build `$1` in shape `$2` on the directories under `$dir`, with
# its output in `$dir/$3.out`, and reads where it begins into `answer`. `$4`, when
# given, is the bytes a recording holds before a release rolls it.
start_feed() {
    feed_out="$dir/$3.out"
    feed_answers=0
    rm -f "$dir/in"
    mkfifo "$dir/in"
    "$(program "$1" feed-directory)" --dir "$dir/driver" --log-dir "$dir/log" \
        --control "$control" --own "$own" --shape "$2" ${4:+--roll-bytes "$4"} \
        <"$dir/in" >"$feed_out" 2>&1 &
    feed_pid=$!
    pids="$pids $feed_pid"
    # The process's read end is open once this open of the write end returns.
    exec 3>"$dir/in"
    answered "the feed of $(named "$1"), shape $2, to say where it begins"
    case "$answer" in
        "open "*) answer=${answer#open } ;;
        *) fail "the feed of $(named "$1"), shape $2, did not open: $answer" ;;
    esac
}
# How many answers the feed process has written: 0 before its output file is there,
# which it opens just after its input.
counted() {
    local count
    count=$(grep -c -E -- "$answers" "$feed_out" 2>/dev/null || true)
    echo "${count:-0}"
}
# Waits for the feed process's next answer and sets `answer` to it; `$1` is what is
# waited for.
answered() {
    local deadline=$((SECONDS + 60))
    feed_answers=$((feed_answers + 1))
    until [ "$(counted)" -ge "$feed_answers" ]; do
        if ! kill -0 "$feed_pid" 2>/dev/null; then
            # Its last answer may have come as it ended: the process answers `end`
            # with `ended` and exits at once, and the shell collects it, between the
            # count above and this check.
            if [ "$(counted)" -ge "$feed_answers" ]; then break; fi
            fail "waited for $1, and the feed's process (pid $feed_pid) ended first; it said: $(cat "$feed_out" 2>/dev/null)"
        fi
        [ "$SECONDS" -lt "$deadline" ] || fail "waited 60 s for $1; the feed's process said: $(cat "$feed_out" 2>/dev/null)"
        sleep 0.05
    done
    answer=$(grep -E -- "$answers" "$feed_out" | sed -n "${feed_answers}p")
}
# Sends the feed process command `$1` and waits for its answer, which must begin with
# the word `$2`; sets `answer` to the rest of it.
ask() {
    echo "$1" >&3
    answered "the feed's answer to \`$1\`"
    case "$answer" in
        "$2") answer="" ;;
        "$2 "*) answer=${answer#"$2 "} ;;
        *) fail "the feed answered \`$1\` with \`$answer\`, and \`$2\` was expected; its output: $(cat "$feed_out")" ;;
    esac
}
# The lines of the feed process's last answer that begin with the word `$1`: its
# `record` lines, or its `note` lines.
lines() {
    awk -v last="$feed_answers" -v want="$1 " -v answers="$answers" '
        $0 ~ answers { seen++; next }
        seen == last - 1 && index($0, want) == 1 { print }
    ' "$feed_out"
}
# Ends the feed process as the server's clean stop does.
end_feed() {
    ask end ended
    exec 3>&-
    ended "$feed_pid" "the feed's process, after its \`end\`,"
    wait "$feed_pid" || fail "the feed's process ended with a failure after its \`end\`; it said: $(cat "$feed_out")"
    forget "$feed_pid"
}
# Kills the feed process with its feed open, as a power cut ends a server.
kill_feed() {
    ask hold held
    # Waited for at once and with the shell's own line about a killed job discarded:
    # SIGKILL ends the process, and the script says that it killed it.
    { kill -KILL "$feed_pid"; wait "$feed_pid"; } 2>/dev/null || true
    exec 3>&-
    forget "$feed_pid"
}
# Appends records `$1` to `$1 + $2 - 1`, adds the line of each to `$dir/appended`, and
# sets `position` to the position after the last.
append() {
    ask "append $1 $2" appended
    position=${answer##* }
    [ "$(lines record | wc -l | tr -d ' ')" = "$2" ] \
        || fail "the feed appended records $1 to $(( $1 + $2 - 1 )) and wrote $(lines record | wc -l | tr -d ' ') lines for them; its output: $(cat "$feed_out")"
    lines record >>"$dir/appended"
}
# Fails unless a read from position `$1` gives every record appended past it, each
# once, in order, at its position; `$2` is who reads.
reads_back() {
    ask "read $1" read
    lines record >"$dir/read"
    awk -v from="$1" '$2 > from' "$dir/appended" >"$dir/expected"
    if ! cmp -s "$dir/read" "$dir/expected"; then
        fail "$2 does not read back from position $1 what was appended past it; the records appended, each as \`record <position after> <length> <digest>\`, and then what was read: $(cat "$dir/expected"; echo ---; cat "$dir/read")"
    fi
    read_count=$(wc -l <"$dir/read" | tr -d ' ')
}

if [ -d "${TMPDIR:-/tmp}" ]; then base=${TMPDIR:-/tmp}; else base=/tmp; fi
root=$(mktemp -d "${base%/}/cavalry-log-directory.XXXXXX")

for directory in $directories; do
    name=${directory%%=*}
    writers=$(echo "${directory#*=}" | tr , ' ')
    first_writer=${writers%% *}
    last_writer=${writers##* }
    endings="stopped killed"
    [ "$first_writer" = "$last_writer" ] || endings="$endings first-killed"
    for ending in $endings; do
        dir="$root/$name-$ending"
        mkdir "$dir"
        : >"$dir/appended"
        kept=""
        next_id=0
        next_record=0
        position=0
        wrote=""

        for writer in $writers; do
            build=${writer%%:*}
            shape=${writer#*:}
            # `killed` kills the last writer's last run, as a crash under the build a
            # server runs now; `first-killed` kills the first writer's, so that the next
            # build recovers the recording it left live, as a server that crashed on its
            # way-back copy and then upgraded again would. Every other run stops.
            how=stopped
            if [ "$ending" = killed ] && [ "$writer" = "$last_writer" ]; then how=killed; fi
            if [ "$ending" = first-killed ] && [ "$writer" = "$first_writer" ]; then how=killed; fi
            step="a ($name, $ending: $build writes in the $shape shape)"
            if [ "$next_id" = 0 ]; then
                start_feed "$build" "$shape" "$build-run-1" 1
                [ "$answer" = 0 ] || fail "$build's feed begins a new directory at position $answer, not 0"
                began "$shape"
                append_records 6
                end_feed
                run_ended
                start_feed "$build" "$shape" "$build-run-2" 1
                [ "$answer" = "$position" ] || fail "$build's second run begins at position $answer, and its first ended at $position"
                began "$shape"
                append_records 5
                release_oldest "$build"
                append_records 5
                release_oldest "$build"
                append_records 4
                wrote="${wrote:+$wrote; }$build wrote $next_record records in two runs to position $position, in the $shape shape"
            else
                start_feed "$build" "$shape" "$build-run" 1
                [ "$answer" = "$position" ] || fail "$build's feed begins at position $answer on the directory that the build before it left at $position"
                began "$shape"
                earliest=$(field "${kept%% *}" 2)
                reads_back "$earliest" "$build's feed, in the $shape shape, on what the build before it wrote,"
                carried=$read_count
                append_records 5
                release_oldest "$build"
                append_records 4
                wrote="${wrote:+$wrote; }$build began there in place, read back the $carried records left from position $earliest, appended 9 more to position $position in the $shape shape and released the oldest recording left"
            fi
            if [ "$how" = stopped ]; then end_feed; else kill_feed; fi
            run_ended
        done
        written=$position
        earliest=$(field "${kept%% *}" 2)
        longest=0
        for entry in $kept; do
            [ "$(field "$entry" 4)" -le "$longest" ] || longest=$(field "$entry" 4)
        done
        say "a. $name, $ending: $wrote; the runs ended $ending, and recordings $(for entry in $kept; do printf '%s ' "$(field "$entry" 1)"; done)are left, from position $earliest"

        step="b ($name, $ending: cavalry run and cavalry replay of N+1 on the directory)"
        "$(program newer cavalry)" run --dir "$dir/driver" --log-dir "$dir/log" >"$dir/N+1-driver.out" 2>&1 &
        driver=$!
        pids="$pids $driver"
        await "$dir/N+1-driver.out" "^cavalry run: driver started on " \
            "\`cavalry run\` of N+1 to start on the directory the older builds wrote" "$driver"
        replayed=""
        for entry in $kept; do
            id=$(field "$entry" 1)
            from_at=$(field "$entry" 2)
            to_at=$(field "$entry" 3)
            "$(program newer cavalry)" replay --dir "$dir/driver" --log-dir "$dir/log" \
                --recording "$id" --from "$from_at" >"$dir/replay-$id.out" 2>"$dir/replay-$id.err" \
                || fail "\`cavalry replay --recording $id --from $from_at\` of N+1 failed on the directory the older builds wrote; it said: $(cat "$dir/replay-$id.err")"
            summary=$(tail -n 1 "$dir/replay-$id.out")
            case "$summary" in
                *" ended at position $to_at: "*" messages from position $from_at") ;;
                *) fail "\`cavalry replay\` of N+1 did not play recording $id from position $from_at to $to_at, where its writer left it; its last line: $summary" ;;
            esac
            replayed="${replayed:+$replayed, }recording $id from $from_at to $to_at"
        done
        stop "$driver" "\`cavalry run\` of N+1"
        grep -q '^cavalry run: closed; ' "$dir/N+1-driver.out" \
            || fail "\`cavalry run\` of N+1 was stopped with SIGTERM on the directory the older builds wrote and did not say \`cavalry run: closed\`"
        say "b. $name, $ending: \`cavalry run\` of N+1 started on the directory as the older builds left it, and \`cavalry replay\` played $replayed"

        step="c ($name, $ending: N+1's feed in the pinned shape)"
        start_feed newer pinned "N+1-pinned"
        [ "$answer" = "$written" ] || fail "N+1's feed begins at position $answer on the directory the older builds wrote, and the last record they answered ends at $written"
        ask "longest" "longest"
        [ "$answer" = "$longest" ] || fail "N+1's feed read the catalog before its driver started and found $answer bytes as the longest term of a recording there, and the longest term among the recordings left is $longest bytes"
        reads_back "$earliest" "N+1's feed, in the pinned shape,"
        from_older=$read_count
        echo "read 0" >&3
        answered "the feed's answer to \`read 0\`"
        [ "$answer" = "failed the feed no longer holds position 0: its earliest recording after it begins at $earliest" ] \
            || fail "N+1's feed was asked to read from position 0, which the older builds purged, and answered \`$answer\`; it must refuse, and say that its earliest recording begins at $earliest"
        append_records 5
        pinned_to=$position
        end_feed
        say "c. $name, $ending: N+1's feed, in the pinned shape, began at $written, found a longest term of $longest bytes in the catalog before its driver started, read back the $from_older records left from position $earliest, each at its position, was refused position 0, and appended 5 records to position $pinned_to"

        step="d ($name, $ending: N+1's feed in the shape the server moved to)"
        start_feed newer moved-to "N+1-moved-to"
        [ "$answer" = "$pinned_to" ] || fail "N+1's feed, in the shape the server moved to, begins at position $answer, and the feed ended at $pinned_to"
        reads_back "$earliest" "N+1's feed, in the shape the server moved to,"
        append_records 5
        moved_to=$position
        end_feed
        say "d. $name, $ending: N+1's feed, with ssc=true and term-length=1m, began at $pinned_to, read back $read_count records from position $earliest and appended 5 records to position $moved_to"

        step="e ($name, $ending: the whole feed, read back)"
        start_feed newer moved-to "N+1-read"
        [ "$answer" = "$moved_to" ] || fail "N+1's feed begins at position $answer, and the feed ended at $moved_to"
        reads_back "$earliest" "N+1's feed"
        end_feed
        say "e. $name, $ending: N+1's feed read back $read_count records from position $earliest to $moved_to, each once, in order, at its position: $from_older that the older builds wrote, and 10 that N+1 wrote, 5 of them in terms of 1 MiB"
    done
done

step="f (stop everything)"
for pid in $pids; do
    stop "$pid" "a program the script started"
done
say "f. every program the script started has ended; the whole run took $(( $(date +%s) - began )) s"
say "ok"
