#!/usr/bin/env bash
#
# E2E verification for the upstream/downstream protocol split (Task 9).
#
# Runs the in-process integration test that exercises all three ingress
# protocols (chat / responses / messages) against three alias modes
# (default / forced-chat / forced-responses) with a mock upstream, and
# verifies the bulk-fallback-on-endpoint-removal logic (Task 4).
#
# This is a self-contained smoke test — it does NOT deploy to the router.
#
# Usage:  bash scripts/e2e_protocol_split.sh
#
set -euo pipefail

# Resolve repo root (scripts/ lives one level below the workspace root).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"

echo "==> Running protocol-split E2E test ($(date -u +%FT%TZ))"
"$CARGO" test --test e2e_protocol_split -- --nocapture

echo "==> Protocol-split E2E verification PASSED"
