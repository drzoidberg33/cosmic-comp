#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Fork-only helper: run cosmic-comp nested (X11 backend) with several outputs, for testing
# multi-output behaviour without leaving the current session.
#
# Usage: scripts/nested.sh [client [args...]]
#   Starts the compositor, waits for its Wayland socket, then launches the client inside it
#   (defaults to cosmic-term). The compositor keeps running until its windows are closed or
#   Ctrl+C is pressed. Start more clients with the socket name that is printed, e.g.
#   `WAYLAND_DISPLAY=wayland-2 cosmic-edit`.
#
#   Clients are not passed to cosmic-comp as arguments on purpose: that runs them in kiosk
#   mode, which skips loading shortcuts and shuts the compositor down as soon as the client
#   exits (COSMIC apps fork into the background and exit immediately).
#
# Environment:
#   COSMIC_X11_OUTPUTS  number of outputs, or comma separated `WxH[@scale][+X+Y]` entries.
#                       Defaults to three 960x540 outputs, two at the bottom and one centred
#                       above them:
#
#                                 +--------+
#                                 |   2    |
#                             +---+----+---+----+
#                             |   0    |   1    |
#                             +--------+--------+
#
#                       Each output is its own host window and the host places them freely.
#                       Arrange the windows like the layout so moving the pointer between them
#                       matches the geometry the nested compositor uses.
#   PROFILE             cargo profile to build and run (default dev-opt)
#   RUST_LOG            log filter (default info). `debug!`/`trace!` are compiled out of
#                       release-based profiles (fastdebug, release).

set -eu

cd "$(dirname "$0")/.."

profile="${PROFILE:-dev-opt}"
case "$profile" in
    dev) target_dir=debug ;;
    *) target_dir="$profile" ;;
esac

cargo build --profile "$profile"

state_dir="${XDG_RUNTIME_DIR:-/tmp}/cosmic-comp-nested"
log="$state_dir/cosmic-comp.log"
mkdir -p "$state_dir/state"

export COSMIC_BACKEND=x11
export COSMIC_X11_OUTPUTS="${COSMIC_X11_OUTPUTS:-960x540+0+540,960x540+960+540,960x540+480+0}"
export RUST_LOG="${RUST_LOG:-info}"
# Keep the nested output layout out of the real ~/.local/state/cosmic-comp/outputs.ron.
export XDG_STATE_HOME="$state_dir/state"
# Never talk to the host cosmic-session over an inherited fd.
unset COSMIC_SESSION_SOCK

: >"$log"
"${CARGO_TARGET_DIR:-target}/$target_dir/cosmic-comp" >"$log" 2>&1 &
comp_pid=$!
trap 'kill "$comp_pid" 2>/dev/null || true' INT TERM EXIT

tail -n +1 -f --pid="$comp_pid" "$log" &

socket=""
tries=0
while [ -z "$socket" ]; do
    if ! kill -0 "$comp_pid" 2>/dev/null; then
        echo "nested.sh: cosmic-comp exited before creating its socket, see $log" >&2
        exit 1
    fi
    tries=$((tries + 1))
    if [ "$tries" -gt 300 ]; then
        echo "nested.sh: timed out waiting for the Wayland socket, see $log" >&2
        exit 1
    fi
    sleep 0.1
    socket="$(sed -n 's/.*Listening on "\(wayland-[0-9]*\)".*/\1/p' "$log" | head -n 1)"
done

if [ "$#" -eq 0 ]; then
    set -- cosmic-term
fi

echo "nested.sh: compositor socket is $socket (log: $log)" >&2
WAYLAND_DISPLAY="$socket" "$@" &

wait "$comp_pid" || true
