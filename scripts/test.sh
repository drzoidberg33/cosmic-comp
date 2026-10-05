#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Fork-only: builds cosmic-comp (dev-opt) and runs the headless integration tests in
# test-harness/. Extra arguments go to `cargo test`, e.g. `scripts/test.sh --test span`.
#
# Environment:
#   COSMIC_TEST_KEEP=1  keep the run directories (logs, screenshots) of passing tests too;
#                       failing tests always keep theirs and print the path.

set -eu

cd "$(dirname "$0")/.."

cargo build --profile dev-opt
exec cargo test --manifest-path test-harness/Cargo.toml "$@"
