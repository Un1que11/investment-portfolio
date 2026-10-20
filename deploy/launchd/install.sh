#!/usr/bin/env bash
# Installs (or reinstalls) the daily portfolio-sync launchd agent for the current user (macOS).
#
#   deploy/launchd/install.sh            build, install and load the agent
#   deploy/launchd/install.sh uninstall  unload and remove it (the database is not touched)
#
# Run it from anywhere inside the repository. See README, "Daily sync".
set -euo pipefail

LABEL="com.portfolio.sync"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/portfolio-sync.log"
DOMAIN="gui/$(id -u)"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "launchd agents exist only on macOS" >&2
    exit 1
fi

# Unload a previously installed agent; it is fine if there is none.
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true

if [[ "${1:-}" == "uninstall" ]]; then
    rm -f "$PLIST"
    echo "removed $LABEL"
    exit 0
elif [[ $# -gt 0 ]]; then
    echo "usage: $0 [uninstall]" >&2
    exit 2
fi

if [[ "$REPO" == *"&"* || "$REPO" == *"|"* ]]; then
    echo "the repository path must not contain '&' or '|': $REPO" >&2
    exit 1
fi

[[ -f "$REPO/config.toml" ]] || { echo "missing $REPO/config.toml" >&2; exit 1; }
[[ -f "$REPO/.env" ]] || echo "note: no $REPO/.env; VWCE prices and the Kraken sync need keys from it"

cargo build --release -p portfolio-sync --manifest-path "$REPO/Cargo.toml"

mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
sed -e "s|__REPO__|$REPO|g" -e "s|__LOG__|$LOG|g" \
    "$REPO/deploy/launchd/portfolio-sync.plist.template" > "$PLIST"
plutil -lint "$PLIST"
launchctl bootstrap "$DOMAIN" "$PLIST"

echo "installed $LABEL: runs daily at 22:30, log in $LOG"
echo "run it now with: launchctl kickstart $DOMAIN/$LABEL"
