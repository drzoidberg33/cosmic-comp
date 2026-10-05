#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Fork-only: benchmarks the working tree against a baseline revision on the headless backend
# and fails if anything regresses beyond the thresholds in test-harness/src/bin/bench.rs.
#
# Usage: scripts/bench.sh [--baseline REV] [bench args...]
#   REV defaults to the `span-baseline` tag (the last commit before the span-outputs feature).
#   Further arguments go to `bench ab`, e.g. `--rounds 5`, `--filter span`, `--json out.json`.
#
# Both sides are built with the `bench-opt` profile. The baseline is built in a git worktree
# under target/bench-baseline with its own target dir, so it doesn't invalidate the normal
# build caches. Close other heavy programs while benchmarking.

set -eu

cd "$(dirname "$0")/.."
root="$(pwd)"

baseline_rev=span-baseline
if [ "${1:-}" = "--baseline" ]; then
    baseline_rev="$2"
    shift 2
fi
rev="$(git rev-parse --verify "$baseline_rev^{commit}")"

bench_dir="$root/target/bench-baseline"
worktree="$bench_dir/worktree"
git worktree prune
if [ -d "$worktree" ]; then
    git -C "$worktree" checkout --detach --quiet "$rev"
else
    mkdir -p "$bench_dir"
    git worktree add --detach "$worktree" "$rev"
fi

echo "bench.sh: building baseline $baseline_rev ($rev)" >&2
CARGO_TARGET_DIR="$bench_dir/target" cargo build --manifest-path "$worktree/Cargo.toml" \
    --profile bench-opt
echo "bench.sh: building candidate (working tree)" >&2
cargo build --profile bench-opt
cargo build --release --manifest-path test-harness/Cargo.toml --bins

exec test-harness/target/release/bench ab \
    --baseline "$bench_dir/target/bench-opt/cosmic-comp" \
    --candidate "$root/target/bench-opt/cosmic-comp" \
    "$@"
