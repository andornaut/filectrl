#!/usr/bin/env bash
# Builds the integration test image and runs the full suite in a container.
set -eu

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
docker build -f "$repo_root/tests/integration/Dockerfile" -t filectrl-integration "$repo_root"
docker run --rm -t filectrl-integration
