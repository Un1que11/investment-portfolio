#!/usr/bin/env bash
# One-command setup for the portfolio dashboard on macOS.
#
#   ./setup.sh             install or update everything and open the dashboard
#   ./setup.sh status      show whether the dashboard and the daily sync are running
#   ./setup.sh uninstall   stop and remove both launchd agents (data and .env are kept)
#
# Safe to run again at any time (e.g. after `git pull`): it rebuilds and reloads.
# What it does: checks the toolchain, fills in .env (asks only for keys that are still empty),
# builds the release binaries, runs the first sync, installs two launchd agents
# (com.portfolio.web keeps the dashboard up; com.portfolio.sync runs daily at 22:30)
# and opens http://127.0.0.1:3000.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOMAIN="gui/$(id -u)"
WEB_LABEL="com.portfolio.web"
SYNC_LABEL="com.portfolio.sync"
AGENTS="$HOME/Library/LaunchAgents"
WEB_PLIST="$AGENTS/$WEB_LABEL.plist"
WEB_LOG="$HOME/Library/Logs/portfolio-web.log"
SYNC_LOG="$HOME/Library/Logs/portfolio-sync.log"
URL="http://127.0.0.1:3000"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
ok() { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
die() { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }
ask_yes() { # ask_yes "question" -> 0 for yes (default yes)
    local reply
    read -r -p "  $1 [Y/n] " reply || true
    [[ -z "$reply" || "$reply" =~ ^[Yy] ]]
}

[[ "$(uname -s)" == "Darwin" ]] || die "this script is for macOS (launchd)"
cd "$REPO"

agent_state() { # agent_state LABEL -> running / loaded / not installed
    if launchctl print "$DOMAIN/$1" >/dev/null 2>&1; then
        if launchctl print "$DOMAIN/$1" | grep -q "state = running"; then echo "running"; else echo "loaded"; fi
    else
        echo "not installed"
    fi
}

case "${1:-}" in
    status)
        bold "Portfolio"
        echo "  dashboard ($WEB_LABEL): $(agent_state "$WEB_LABEL")  → $URL"
        echo "  daily sync ($SYNC_LABEL): $(agent_state "$SYNC_LABEL")"
        if curl -fsS "$URL/health" >/dev/null 2>&1; then ok "dashboard answers"; else warn "dashboard does not answer"; fi
        echo "  logs: $WEB_LOG, $SYNC_LOG"
        exit 0
        ;;
    uninstall)
        launchctl bootout "$DOMAIN/$WEB_LABEL" 2>/dev/null || true
        rm -f "$WEB_PLIST"
        deploy/launchd/install.sh uninstall
        ok "removed both agents; portfolio.db, config.toml and .env are untouched"
        exit 0
        ;;
    "") ;;
    *) echo "usage: $0 [status|uninstall]" >&2; exit 2 ;;
esac

[[ "$REPO" != *"&"* && "$REPO" != *"|"* ]] || die "the repository path must not contain '&' or '|': $REPO"

bold "1/6 Toolchain"
if ! command -v cargo >/dev/null 2>&1 && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
    if ask_yes "Rust is not installed. Install it with rustup now?"; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        die "Rust is needed: https://rustup.rs"
    fi
fi
ok "$(cargo --version)"

bold "2/6 Settings (.env)"
[[ -f config.toml ]] || die "missing config.toml"
if [[ ! -f .env ]]; then
    cp .env.example .env
    chmod 600 .env
    ok "created .env from .env.example"
fi
# The monthly summary uses the same Telegram bot as f1-brief: reuse its settings when that
# checkout sits next to this one.
F1_ENV="$REPO/../f1-brief/.env"
if [[ -f "$F1_ENV" ]]; then
    for key in TELEGRAM_BOT_TOKEN TELEGRAM_CHAT_ID; do
        value="$(grep -E "^$key=" "$F1_ENV" | head -1 | cut -d= -f2- || true)"
        current="$(grep -E "^$key=" .env | head -1 | cut -d= -f2- || true)"
        if [[ -n "$value" && -z "$current" ]]; then
            tmp="$(mktemp)"
            grep -vE "^$key=" .env >"$tmp" || true
            printf '%s=%s\n' "$key" "$value" >>"$tmp"
            mv "$tmp" .env
            ok "$key copied from ../f1-brief/.env"
        fi
    done
fi
# For every KEY= in .env.example that is empty in .env, show its comment and ask for a value.
comment=""
while IFS= read -r line <&3 || [[ -n "$line" ]]; do
    if [[ "$line" =~ ^#\ ?(.*) ]]; then
        comment+="    ${BASH_REMATCH[1]}"$'\n'
        continue
    fi
    if [[ "$line" =~ ^([A-Z0-9_]+)= ]]; then
        key="${BASH_REMATCH[1]}"
        current="$(grep -E "^$key=" .env | head -1 | cut -d= -f2- || true)"
        if [[ -z "$current" ]]; then
            printf '\n  %s is empty.\n%s' "$key" "$comment"
            read -r -s -p "  Value (Enter to skip): " value || true
            echo
            if [[ -n "$value" ]]; then
                if grep -qE "^$key=" .env; then
                    tmp="$(mktemp)"
                    awk -v k="$key" -v v="$value" 'BEGIN{FS=OFS="="} $1==k{print k"="v; next} {print}' .env >"$tmp" && mv "$tmp" .env
                else
                    printf '%s=%s\n' "$key" "$value" >>.env
                fi
                ok "$key saved"
            else
                warn "$key skipped (you can fill it in .env later and re-run ./setup.sh)"
            fi
        else
            ok "$key is set"
        fi
    fi
    comment=""
done 3<.env.example
chmod 600 .env

bold "3/6 Build"
cargo build --release -p portfolio-web -p portfolio-sync
ok "built target/release/portfolio-web and portfolio-sync"

bold "4/6 First sync (Kraken ledger + today's snapshot)"
if target/release/portfolio-sync; then
    ok "synced"
else
    warn "the sync reported a problem (often: no data imported yet, or a missing key) — the dashboard still works"
fi

bold "5/6 launchd agents"
mkdir -p "$AGENTS" "$HOME/Library/Logs"
launchctl bootout "$DOMAIN/$WEB_LABEL" 2>/dev/null || true
sed -e "s|__REPO__|$REPO|g" -e "s|__LOG__|$WEB_LOG|g" \
    deploy/launchd/portfolio-web.plist.template >"$WEB_PLIST"
plutil -lint -s "$WEB_PLIST"
launchctl bootstrap "$DOMAIN" "$WEB_PLIST"
ok "$WEB_LABEL: dashboard starts at login and restarts if it stops"
deploy/launchd/install.sh >/dev/null
ok "$SYNC_LABEL: daily sync at 22:30"

bold "6/6 Dashboard"
for _ in $(seq 1 30); do
    curl -fsS "$URL/health" >/dev/null 2>&1 && break
    sleep 1
done
if curl -fsS "$URL/health" >/dev/null 2>&1; then
    ok "running at $URL"
    open "$URL"
else
    warn "the dashboard did not answer yet; see $WEB_LOG"
fi

cat <<EOF

Done. Next:
  • Import your XTB history on $URL/import (xStation → History → Export to Excel)
  • Add your gold coins on $URL/gold
  • ./setup.sh status     check both agents
  • ./setup.sh            again after git pull (rebuilds and reloads)
EOF
