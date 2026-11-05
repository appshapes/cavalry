#!/usr/bin/env bash
# The udp pair's wait for each other (card 212). The two udp jobs of .github/workflows/ci.yml
# call this name; the wait is scripts/wait-for-partner-job.py, which says why it exists and
# when it ends, and is Python so that scripts/wait-for-partner-job-test.py can test it
# against a fake API.
#
#     scripts/wait-for-partner-job.sh "bench (udp peer)"
set -euo pipefail
exec python3 -I "$(dirname "$0")/wait-for-partner-job.py" "$@"
