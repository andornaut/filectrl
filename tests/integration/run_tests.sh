#!/usr/bin/env bash
# Runs every test_*.sh suite in this directory. Works directly on a host with
# tmux + a built binary, and is the entrypoint of the Docker image (run.sh).
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
status=0
for suite in "$here"/test_*.sh; do
    echo "== ${suite##*/} =="
    bash "$suite" || status=1
    echo
done
exit "$status"
