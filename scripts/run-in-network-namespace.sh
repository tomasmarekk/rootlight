#!/usr/bin/env bash
# Runs a repository script as the hosted runner user in a fresh no-interface namespace.
# Root creates the namespace, then all capabilities are dropped before project code runs.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    printf 'usage: %s <script> [arguments...]\n' "$0" >&2
    exit 2
fi
if [[ "$(id -u)" -ne 0 ]]; then
    printf 'network namespace setup requires root\n' >&2
    exit 1
fi
if [[ -z "${SUDO_UID:-}" || -z "${SUDO_GID:-}" ]]; then
    printf 'SUDO_UID and SUDO_GID are required to drop privileges\n' >&2
    exit 1
fi

runner_home="${RUNNER_HOME:?RUNNER_HOME is required}"
runner_path="${RUNNER_PATH:?RUNNER_PATH is required}"

exec unshare --net -- \
    setpriv \
        --reuid "$SUDO_UID" \
        --regid "$SUDO_GID" \
        --clear-groups \
        --bounding-set=-all \
        --inh-caps=-all \
        --ambient-caps=-all \
        --no-new-privs \
        env \
            HOME="$runner_home" \
            PATH="$runner_path" \
            CARGO_HOME="$runner_home/.cargo" \
            RUSTUP_HOME="$runner_home/.rustup" \
            CARGO_NET_OFFLINE=true \
            CARGO_HTTP_TIMEOUT=1 \
            CARGO_NET_RETRY=0 \
            REQUIRE_BLOCKED_EGRESS=1 \
            bash "$@"
