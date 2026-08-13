#!/usr/bin/env bash
# Runs every test_*.sh suite in this directory. Needs tmux and a release build.
#
#   run_tests.sh              against the working tree
#   run_tests.sh --committed  against `git archive HEAD`, which is what CI
#                             checks out
#
# --committed takes the fixtures from the commit and the suites from the
# working tree, so an edited suite can be checked against them. It is what
# catches a test that depends on something the repository cannot hold: git
# stores regular files, symlinks and gitlinks, so a named pipe or a setuid bit
# exists only where someone made it, and an ignore rule can keep a fixture out
# of a checkout entirely.
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

run_committed() {
    local repo_root tree
    repo_root="$(cd "$here/../.." && pwd)"
    git -C "$repo_root" rev-parse --git-dir > /dev/null 2>&1 || {
        echo "--committed needs a git repository at $repo_root" >&2
        exit 1
    }
    tree="$(mktemp -d)" || exit 1
    trap 'rm -rf "$tree"' EXIT
    git -C "$repo_root" archive HEAD -- fixtures | tar -x -C "$tree" || exit 1
    mkdir -p "$tree/tests"
    cp -r "$here" "$tree/tests/integration" || exit 1
    # The extracted tree has no target/, so the binary has to be named
    # explicitly; the suites resolve everything else relative to themselves.
    FILECTRL_BIN="${FILECTRL_BIN:-$repo_root/target/release/filectrl}" \
        "$tree/tests/integration/run_tests.sh"
}

case "${1:-}" in
--committed)
    run_committed
    exit "$?"
    ;;
"") ;;
*)
    echo "usage: ${BASH_SOURCE[0]##*/} [--committed]" >&2
    exit 1
    ;;
esac

status=0
for suite in "$here"/test_*.sh; do
    echo "== ${suite##*/} =="
    bash "$suite" || status=1
    echo
done
exit "$status"
