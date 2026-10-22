#!/usr/bin/env bash
# One-command setup for the portfolio dashboard on macOS.
#
#   ./setup.sh             install or update everything and open the dashboard
#   ./setup.sh status      show whether the dashboard and the daily sync are running
#   ./setup.sh uninstall   stop and remove both launchd agents (data and .env are kept)
#
#   ./setup.sh --docker          the same in Docker instead (compose.yaml): build, start, open
#   ./setup.sh --docker status   containers, health, and a warning if launchd agents are loaded too
#   ./setup.sh --docker logs     follow both containers' logs (extra arguments go to compose)
#   ./setup.sh --docker down     stop the containers (the database volume stays)
#
# Safe to run again at any time (e.g. after `git pull`): it rebuilds and reloads.
# What it does: checks the toolchain, fills in .env (asks only for keys that are still empty),
# builds the release binaries, runs the first sync, installs two launchd agents
# (com.portfolio.web keeps the dashboard up; com.portfolio.sync runs daily at 22:30)
# and opens http://127.0.0.1:3000.
#
# Only one of the two runs at a time, so the daily sync never runs twice: --docker unloads the
# launchd agents, and the plain setup stops the containers. The two keep separate databases
# (./portfolio.db and the portfolio-data volume); --docker offers to copy ./portfolio.db into
# a new, empty volume.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOMAIN="gui/$(id -u)"
WEB_LABEL="com.portfolio.web"
SYNC_LABEL="com.portfolio.sync"
AGENTS="$HOME/Library/LaunchAgents"
WEB_PLIST="$AGENTS/$WEB_LABEL.plist"
SYNC_PLIST="$AGENTS/$SYNC_LABEL.plist"
WEB_LOG="$HOME/Library/Logs/portfolio-web.log"
SYNC_LOG="$HOME/Library/Logs/portfolio-sync.log"
URL="http://127.0.0.1:3000"
# Left by --docker and removed once the containers are down: tells the plain setup that
# containers may still exist when Docker is not running and cannot be asked.
DOCKER_MARK="$REPO/.docker-mode"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
ok() { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
die() { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }
ask_yes() { # ask_yes "question" -> 0 for yes (default yes)
    local reply
    read -r -p "  $1 [Y/n] " reply || true
    [[ -z "$reply" || "$reply" =~ ^[Yy] ]]
}
usage() {
    echo "usage: $0 [status|uninstall]" >&2
    echo "       $0 --docker [status|logs|down]" >&2
    exit 2
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

launchd_installed() { # either agent is loaded, or its plist would load it at the next login
    [[ -f "$WEB_PLIST" || -f "$SYNC_PLIST" ]] \
        || [[ "$(agent_state "$WEB_LABEL")" != "not installed" ]] \
        || [[ "$(agent_state "$SYNC_LABEL")" != "not installed" ]]
}

unload_launchd() { # stop both agents and remove their plists, so they do not come back at login
    launchctl bootout "$DOMAIN/$WEB_LABEL" 2>/dev/null || true
    rm -f "$WEB_PLIST"
    deploy/launchd/install.sh uninstall >/dev/null
}

docker_running() { command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; }

# IDs of this project's containers, running or stopped (empty when there are none).
compose_containers() { docker compose ps -a -q 2>/dev/null || true; }

require_docker() {
    command -v docker >/dev/null 2>&1 || die "Docker is not installed: https://www.docker.com/products/docker-desktop/"
    docker info >/dev/null 2>&1 || die "Docker is not running: start Docker Desktop and try again"
    docker compose version >/dev/null 2>&1 || die "docker compose is missing; update Docker Desktop"
}

# Creates .env if needed, reuses the Telegram settings of a ../f1-brief checkout, then asks for
# every KEY= in .env.example that is still empty in .env, showing the comment above it.
fill_env() {
    [[ -f config.toml ]] || die "missing config.toml"
    if [[ ! -f .env ]]; then
        cp .env.example .env
        chmod 600 .env
        ok "created .env from .env.example"
    fi
    # The monthly summary uses the same Telegram bot as f1-brief: reuse its settings when that
    # checkout sits next to this one.
    local f1_env="$REPO/../f1-brief/.env" key value current tmp line comment=""
    if [[ -f "$f1_env" ]]; then
        for key in TELEGRAM_BOT_TOKEN TELEGRAM_CHAT_ID; do
            value="$(grep -E "^$key=" "$f1_env" | head -1 | cut -d= -f2- || true)"
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
}

wait_for_dashboard() { # wait_for_dashboard LOG_HINT
    for _ in $(seq 1 30); do
        curl -fsS "$URL/health" >/dev/null 2>&1 && break
        sleep 1
    done
    if curl -fsS "$URL/health" >/dev/null 2>&1; then
        ok "running at $URL"
        open "$URL"
    else
        warn "the dashboard did not answer yet; see $1"
    fi
}

# Copies ./portfolio.db into the portfolio-data volume when the volume has no database yet, so
# switching to Docker keeps the coins and imports. Run only after the launchd agents are
# stopped, so nothing writes the file during the copy; its -wal file, if SQLite left one, goes
# along. An existing database in the volume is never overwritten.
copy_db_into_volume() {
    [[ -f portfolio.db ]] || return 0
    # `docker compose run` of web as its own user (uid 10001, owner of /data); --no-deps, and
    # no ports are published by `run`.
    local run=(docker compose run --rm --no-deps -T -v "$REPO:/import:ro" --entrypoint /bin/sh web -c)
    if "${run[@]}" 'test -e /data/portfolio.db'; then
        ok "the portfolio-data volume already has a database; ./portfolio.db is left as it is"
        return 0
    fi
    if ask_yes "Copy ./portfolio.db (the launchd setup's data) into the new Docker volume?"; then
        "${run[@]}" 'cp /import/portfolio.db /data/portfolio.db && if [ -s /import/portfolio.db-wal ]; then cp /import/portfolio.db-wal /data/portfolio.db-wal; fi'
        ok "copied ./portfolio.db into the volume (the original stays)"
    else
        warn "not copied: the Docker dashboard starts with an empty database"
    fi
}

MODE="launchd"
if [[ "${1:-}" == "--docker" ]]; then
    MODE="docker"
    shift
fi
ACTION="${1:-}"

if [[ "$MODE" == "launchd" ]]; then
    [[ $# -le 1 ]] || usage
    case "$ACTION" in
        status)
            bold "Portfolio"
            echo "  dashboard ($WEB_LABEL): $(agent_state "$WEB_LABEL")  → $URL"
            echo "  daily sync ($SYNC_LABEL): $(agent_state "$SYNC_LABEL")"
            if curl -fsS "$URL/health" >/dev/null 2>&1; then ok "dashboard answers"; else warn "dashboard does not answer"; fi
            if docker_running && [[ -n "$(compose_containers)" ]]; then
                warn "the Docker containers exist too; run ./setup.sh again to stop them, or ./setup.sh uninstall"
            fi
            echo "  logs: $WEB_LOG, $SYNC_LOG"
            exit 0
            ;;
        uninstall)
            unload_launchd
            ok "removed both agents; portfolio.db, config.toml and .env are untouched"
            exit 0
            ;;
        "") ;;
        *) usage ;;
    esac
else
    case "$ACTION" in
        status)
            [[ $# -le 1 ]] || usage
            require_docker
            bold "Portfolio (Docker)"
            docker compose ps
            if curl -fsS "$URL/health" >/dev/null 2>&1; then ok "dashboard answers at $URL"; else warn "dashboard does not answer"; fi
            if launchd_installed; then
                warn "launchd agents are installed too; run ./setup.sh --docker again to unload them"
            fi
            exit 0
            ;;
        logs)
            require_docker
            shift
            exec docker compose logs -f --tail=100 "$@"
            ;;
        down)
            [[ $# -le 1 ]] || usage
            require_docker
            docker compose down
            rm -f "$DOCKER_MARK"
            ok "containers stopped; the database stays on the portfolio-data volume"
            exit 0
            ;;
        "") ;;
        *) usage ;;
    esac
fi

[[ "$REPO" != *"&"* && "$REPO" != *"|"* ]] || die "the repository path must not contain '&' or '|': $REPO"

if [[ "$MODE" == "docker" ]]; then
    bold "1/5 Docker"
    require_docker
    ok "$(docker compose version)"

    bold "2/5 Settings (.env)"
    fill_env

    # Build before anything is stopped, so a failed build leaves the launchd setup running.
    bold "3/5 Build the image"
    docker compose build
    ok "built image portfolio"

    bold "4/5 Switch from launchd"
    if launchd_installed; then
        unload_launchd
        ok "unloaded $WEB_LABEL and $SYNC_LABEL (./setup.sh without --docker brings them back)"
    else
        ok "no launchd agents installed"
    fi
    copy_db_into_volume

    bold "5/5 Start"
    # The backup service writes here; owner-only, since each copy is the whole portfolio.
    mkdir -p backups
    chmod 700 backups
    touch "$DOCKER_MARK"
    # --wait returns once web's health check passes (or fails it after the timeout).
    if docker compose up -d --wait --wait-timeout 120; then
        ok "web and sync are up; the daily sync runs at 22:30 in the sync container"
        wait_for_dashboard "./setup.sh --docker logs"
    else
        warn "the containers did not become healthy; see ./setup.sh --docker logs"
    fi

    cat <<EOF

Done. Next:
  • Import your XTB history on $URL/import (xStation → History → Export to Excel)
  • Add your gold coins on $URL/gold
  • ./setup.sh --docker status   check the containers
  • ./setup.sh --docker logs     follow their logs
  • docker compose run --rm backup   dated copy of the database in ./backups/
  • ./setup.sh --docker          again after git pull (rebuilds and restarts)
  • ./setup.sh                   back to launchd (stops the containers)
EOF
    exit 0
fi

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
fill_env

bold "3/6 Build"
cargo build --release -p portfolio-web -p portfolio-sync
ok "built target/release/portfolio-web and portfolio-sync"

# Before the first sync: the containers' daily sync must not run next to the launchd one, and
# the containers' web holds port 3000. When Docker is not running, warn only if --docker was
# used here, rather than every time Docker Desktop happens to be closed.
if command -v docker >/dev/null 2>&1; then
    if docker_running; then
        if [[ -n "$(compose_containers)" ]]; then
            docker compose down
            ok "stopped the Docker containers (./setup.sh --docker brings them back)"
            warn "the Docker volume's database is not copied back to ./portfolio.db; for that, run"
            warn "docker compose run --rm backup, then copy the new file from ./backups/ over portfolio.db"
        fi
        rm -f "$DOCKER_MARK"
    elif [[ -f "$DOCKER_MARK" ]]; then
        warn "Docker is not running, so its containers could not be checked. If you used"
        warn "./setup.sh --docker, start Docker and run ./setup.sh --docker down: otherwise the"
        warn "containers come back with Docker and the daily sync runs twice"
    fi
fi

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
wait_for_dashboard "$WEB_LOG"

cat <<EOF

Done. Next:
  • Import your XTB history on $URL/import (xStation → History → Export to Excel)
  • Add your gold coins on $URL/gold
  • ./setup.sh status     check both agents
  • ./setup.sh            again after git pull (rebuilds and reloads)
  • ./setup.sh --docker   run it in Docker instead (stops these agents)
EOF
