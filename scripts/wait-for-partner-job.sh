#!/usr/bin/env bash
# Wait until the run's partner job is in progress (card 212). `bench (udp)` and
# `bench (udp peer)` are the two ends of one measurement over the cable, each on its own
# host, and the harness on each end waits ten minutes for the other to connect
# (docs/how-to/run-the-bench.md, step 5). Both jobs queue once `bench` and `bench-grip`
# end, but each host's queue is its own: the partner can sit behind another run's job on
# its host for longer than those ten minutes, and the end that started first dies
# waiting, as runs 42017993331, 42051225401 and 42051752280 did on 2026-11-05. So each
# job runs this first, doing nothing until the run's other job is in progress, read from
# the run's jobs through the API every 20 s; then both reach the harness within one
# poll of each other, and the harness's own wait holds.
#
#     scripts/wait-for-partner-job.sh "bench (udp peer)"
#
# Needs GH_TOKEN with `actions: read`, and GITHUB_API_URL, GITHUB_REPOSITORY and
# GITHUB_RUN_ID, which every job has. Exits 0 once the partner is in progress. Exits 1
# when the partner completed before this job reached it, so the pair cannot meet, or
# when it has not started within the bound, 45 minutes, which is a host whose runner is
# gone or a queue longer than any this repository has seen; the message says which.
set -euo pipefail
partner=${1:?"the partner job's name, as the workflow names it"}
bound=$((45 * 60))
waited=0
# The partner's status in the run's latest attempt, or empty when it is not listed.
read_status='import json, sys
name = sys.argv[1]
jobs = json.load(sys.stdin).get("jobs", [])
print(next((j["status"] for j in jobs if j["name"] == name), ""))'
partner_status() {
    curl -sS --max-time 20 \
        -H "Authorization: Bearer $GH_TOKEN" \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        "$GITHUB_API_URL/repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/jobs?per_page=100" \
        | python3 -c "$read_status" "$partner"
}
while :; do
    status=$(partner_status 2>/dev/null || echo unknown)
    case "$status" in
        in_progress)
            echo "wait-for-partner-job: $partner is in progress after ${waited} s"
            exit 0 ;;
        completed)
            echo "wait-for-partner-job: $partner completed before this job reached it, so the pair cannot meet; re-run both jobs when both hosts are free (card 212)" >&2
            exit 1 ;;
        *)
            echo "wait-for-partner-job: $partner is ${status:-not listed yet} after ${waited} s" ;;
    esac
    if [ "$waited" -ge "$bound" ]; then
        echo "wait-for-partner-job: $partner did not start within $((bound / 60)) minutes; is its host's runner online, or its queue that long? (card 212)" >&2
        exit 1
    fi
    sleep 20
    waited=$((waited + 20))
done
