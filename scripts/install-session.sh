#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
#
# Fork-only: installs the release build as a separate "COSMIC (span-outputs)" login session.
# The stock COSMIC session and /usr/bin/cosmic-comp (owned by the cosmic-comp package) are not
# touched, so the stock session stays available as a fallback.
#
# Usage: scripts/install-session.sh [install|uninstall]
#   install    copies target/release/cosmic-comp to /opt/cosmic-comp-custom/bin and adds the
#              session (build first with `cargo build --release`). Re-run after rebuilding;
#              an existing wrapper (/usr/local/bin/start-cosmic-custom) is kept as is.
#   uninstall  removes everything this script installed.
#
# How it works: cosmic-session starts `cosmic-comp` from PATH. The session's wrapper puts
# /opt/cosmic-comp-custom/bin first on PATH and then runs the stock /usr/bin/start-cosmic.

set -eu

cd "$(dirname "$0")/.."

prefix=/opt/cosmic-comp-custom
wrapper=/usr/local/bin/start-cosmic-custom
desktop=/usr/share/wayland-sessions/cosmic-custom.desktop

case "${1:-install}" in
    install)
        bin=target/release/cosmic-comp
        if [ ! -x "$bin" ]; then
            echo "install-session.sh: $bin not found, run \`cargo build --release\` first" >&2
            exit 1
        fi
        echo "Installing $(git rev-parse --short HEAD) ($(git branch --show-current))"
        sudo install -Dm0755 "$bin" "$prefix/bin/cosmic-comp"
        # Keep an existing wrapper: it may carry local additions, e.g. environment variables
        # like COSMIC_DISABLE_OVERLAY_SCANOUT=1.
        if [ -e "$wrapper" ]; then
            echo "Keeping existing $wrapper"
        else
            sudo tee "$wrapper" >/dev/null <<'EOF'
#!/bin/sh
# Installed by cosmic-comp fork's scripts/install-session.sh. Runs the stock COSMIC session with
# the custom cosmic-comp first on PATH; cosmic-session starts `cosmic-comp` from PATH.
# Add environment variables for the compositor above the PATH line.
export PATH="/opt/cosmic-comp-custom/bin:$PATH"
exec /usr/bin/start-cosmic "$@"
EOF
            sudo chmod 0755 "$wrapper"
        fi
        sudo tee "$desktop" >/dev/null <<'EOF'
[Desktop Entry]
Name=COSMIC (span-outputs)
Comment=COSMIC with the custom cosmic-comp build in /opt/cosmic-comp-custom
Exec=/usr/local/bin/start-cosmic-custom
Type=Application
DesktopNames=COSMIC
EOF
        echo "Installed. Log out and pick \"COSMIC (span-outputs)\" on the login screen."
        ;;
    uninstall)
        sudo rm -f "$desktop" "$wrapper"
        sudo rm -rf "$prefix"
        echo "Removed the custom session and binary."
        ;;
    *)
        echo "usage: $0 [install|uninstall]" >&2
        exit 2
        ;;
esac
