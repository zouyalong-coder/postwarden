#!/bin/sh
# Shared launcher for local development and the container image.
set -eu

usage() {
    cat <<'EOF'
Usage: ./scripts/postwarden.sh init [admin-login]
       ./scripts/postwarden.sh start

init   Build the local application, create .env if missing, apply database
       migrations, and interactively initialize the administrator.
start  Run the same service entrypoint used by the container image.
EOF
}

command_name=${1:-help}
case "$command_name" in
    init|start) ;;
    help|-h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
esac
if [ "$command_name" = start ] && [ "$#" -ne 1 ]; then
    usage >&2
    exit 2
fi
if [ "$command_name" = init ] && [ "$#" -gt 2 ]; then
    usage >&2
    exit 2
fi

if [ -n "${POSTWARDEN_BINARY:-}" ]; then
    binary=$POSTWARDEN_BINARY
    config_file=${POSTWARDEN_CONFIG_FILE:-}
    local_build=false
else
    root_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
    cd "$root_dir"
    binary=$root_dir/target/debug/postwarden
    config_file=${POSTWARDEN_CONFIG_FILE:-$root_dir/.env}
    local_build=true
fi

if [ "$command_name" = init ] && [ "$local_build" = true ]; then
    for program in cargo npm openssl; do
        if ! command -v "$program" >/dev/null 2>&1; then
            printf 'Missing %s. Install it, then run this command again.\n' "$program" >&2
            exit 1
        fi
    done
    if [ ! -f "$config_file" ]; then
        umask 077
        master_key=$(openssl rand -base64 32)
        cat > "$config_file" <<EOF
# Local development settings. Keep this file and its master key private.
POSTWARDEN_LISTEN=127.0.0.1:8080
DATABASE_URL=sqlite://data/postwarden.db
POSTWARDEN_WEB_DIST=frontend/dist
POSTWARDEN_COOKIE_SECURE=false
POSTWARDEN_PENDING_DAYS=7
POSTWARDEN_CONTENT_DAYS=7
POSTWARDEN_AUDIT_DAYS=90
POSTWARDEN_MASTER_KEY=$master_key
EOF
        printf 'Created %s with a new persistent master key (mode 600).\n' "$config_file"
    else
        printf 'Using existing config %s; master key was not changed.\n' "$config_file"
    fi
    printf 'Installing frontend dependencies and building the management page...\n'
    npm --prefix frontend ci
    npm --prefix frontend run build
    printf 'Building the Rust service...\n'
    cargo build --locked
fi

if [ -n "$config_file" ]; then
    if [ ! -f "$config_file" ]; then
        printf 'Configuration file %s is missing. Run init first.\n' "$config_file" >&2
        exit 1
    fi
    set -a
    # shellcheck disable=SC1090
    . "$config_file"
    set +a
fi

if [ -z "${POSTWARDEN_MASTER_KEY:-}" ]; then
    printf 'POSTWARDEN_MASTER_KEY is missing. Run init or provide it in the environment.\n' >&2
    exit 1
fi
if [ ! -x "$binary" ]; then
    printf 'Service binary %s is missing. Run init first.\n' "$binary" >&2
    exit 1
fi

case "$command_name" in
    init)
        login_name=${2:-admin}
        printf 'Applying database migrations and checking administrator %s...\n' "$login_name"
        "$binary" setup "$login_name"
        if [ "$local_build" = true ]; then
            printf '\nReady. Start the service with ./scripts/postwarden.sh start\n'
        else
            printf '\nInitialization complete. Start the service with the container default command.\n'
        fi
        ;;
    start)
        printf 'Starting PostWarden...\n'
        exec "$binary"
        ;;
esac
