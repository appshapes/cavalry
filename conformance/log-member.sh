#!/usr/bin/env bash
# usage: conformance/log-member.sh <cluster directory>
#
# Prints "<member id> <recording id>": the member a client's committed log round reads
# beside, of the three that a runner's run.sh started under <cluster directory> as
# member-0, member-1 and member-2, and the recording that holds that member's log.
#
# A round expects one entry at position 0 in one recording that it then follows, which
# is a member whose log is one recording that begins at 0. A member that elects is such
# a member unless it joined late: a follower that was slow at its start catches up
# through a replay and holds its log in two recordings (ADR 0103), and a
# round beside it would read its one entry from a recording that has stopped. So this
# script reads each member's file of its log's recordings, `log-recordings-<member id>`
# in its driver directory (spec/cluster.md, "The log's recordings"): the live recording
# id at byte 16, where it begins at byte 24, and how many recordings lie below it at
# byte 32, each little-endian. It takes the first of members 1, 0 and 2 whose live
# recording begins at 0 with none below it, and asks again every 100 ms for 60 s, since
# a member writes its file as it joins the log. The leader is always such a member, so
# a cluster that has formed has one.
set -euo pipefail
cluster=$1

# The integer that od printed as one hex number on this function's input, signed at
# eight bytes. od's own signed decimal is not the same everywhere: `od -t d8` on macOS
# 26.6 prints the low byte alone, 0 for 4096 and -128 for 128, where its hex is right.
number() {
    echo $((16#$(tr -d ' \n')))
}

# The integer of `$2` bytes at offset `$1` of the copy of a member's file, in the host's
# order, which is little-endian on every host the runners use.
field() {
    od -An -t "x$2" -j "$1" -N "$2" "$snapshot" | number
}

# A member writes its file whole under `log-recordings-<member id>.tmp` and renames it
# over the file (spec/cluster.md, "The log's recordings"), so each open of the name finds
# one whole file, but two opens may find two files: three reads of the name could take
# the live recording id from one file and where it begins from the next. So the script
# opens a member's file once per read, as the spec's reader does, copies its 40-byte
# header, and takes every field from the copy.
snapshot=$(mktemp "${TMPDIR:-/tmp}/cavalry-log-member.XXXXXX")
trap 'rm -f "$snapshot"' EXIT

# Copies the header of member `$1`'s file to the snapshot, with one open; fails when the
# member has no file, as before it first writes one and after it closes.
copy() {
    head -c 40 "$cluster/member-$1/log-recordings-$1" >"$snapshot"
}

# The bytes the copy holds: 40, or fewer for a file shorter than its header, which no
# member writes, since the file it renames is whole.
length() {
    wc -c <"$snapshot" | tr -d ' '
}

# The reader is held to eight bytes that are 4096 in little-endian order before it
# reads a member's file, so a host that reads them otherwise chooses no member.
read_4096=$(printf '\000\020\000\000\000\000\000\000' | od -An -t x8 | number) || read_4096=nothing
if [ "$read_4096" != 4096 ]; then
    {
        echo "conformance: this host reads $read_4096 where the bytes 00 10 00 00 00 00 00 00 hold 4096, so conformance/log-member.sh cannot read a member's file of its log's recordings."
        echo "  Run the pipe that reads them, in the script above this message: od prints 1000 on a little-endian host, with or without zeros before it. When it prints anything else, the script's functions number and field are what to change."
    } >&2
    exit 1
fi

# Sixty seconds by the shell's clock: a round's own reads take time that a count of
# rounds would add to the bound.
SECONDS=0
while [ "$SECONDS" -lt 60 ]; do
    for id in 1 0 2; do
        # A member with no file yet, or a file shorter than its header, names no
        # recording this pass; the next pass reads the name again, and the report after
        # 60 s says which it was.
        copy "$id" 2>/dev/null && [ "$(length)" -ge 40 ] || continue
        if [ "$(field 24 8)" = 0 ] && [ "$(field 32 4)" = 0 ]; then
            echo "$id $(field 16 8)"
            exit 0
        fi
    done
    sleep 0.1
done
echo "conformance: no member under $cluster holds its log in one recording that begins at 0 after 60 s:" >&2
for id in 0 1 2; do
    file="$cluster/member-$id/log-recordings-$id"
    if [ ! -e "$file" ]; then
        echo "  member $id: no $file, so the member names no recording of its log" >&2
    elif ! copy "$id"; then
        echo "  member $id: $file is there and this script cannot read it, as head says above" >&2
    elif [ "$(length)" -lt 40 ]; then
        echo "  member $id: $file holds $(length) bytes, fewer than its 40-byte header, so it is no file a member wrote whole (spec/cluster.md, \"The log's recordings\")" >&2
    else
        echo "  member $id: live recording $(field 16 8) from position $(field 24 8), $(field 32 4) below it" >&2
    fi
done
exit 1
