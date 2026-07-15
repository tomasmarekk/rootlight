#!/usr/bin/env bash
# Proves required security controls fail closed against isolated fixtures.

set -euo pipefail

run_xtask() {
    cargo run --locked --quiet --package xtask -- "$@"
}

expect_failure() {
    local marker="$1"
    shift
    local output
    local status

    set +e
    output="$("$@" 2>&1)"
    status=$?
    set -e
    if [[ $status -eq 0 ]]; then
        printf 'expected command to fail: %q ' "$@" >&2
        printf '\n' >&2
        return 1
    fi
    if ! grep --fixed-strings --quiet "$marker" <<<"$output"; then
        printf 'expected failure marker %s, observed:\n%s\n' "$marker" "$output" >&2
        return 1
    fi
}

expect_failure \
    ARCH_FORBIDDEN_EDGE \
    run_xtask architecture-check --fixture-root tests/fixtures/architecture/forbidden-edge
expect_failure \
    POLICY_UNSAFE \
    run_xtask unsafe-check --fixture-root tests/fixtures/unsafe-rejected

secret_root="$(mktemp -d)"
cleanup() {
    rm -rf "$secret_root"
}
trap cleanup EXIT
git -C "$secret_root" init --quiet
git -C "$secret_root" config user.email rootlight-ci@example.invalid
git -C "$secret_root" config user.name rootlight-ci
git -C "$secret_root" config core.autocrlf false
printf 'glpat-%s%s\n' '0123456789' 'abcdefghijkl' > "$secret_root/synthetic-secret.txt"
git -C "$secret_root" add synthetic-secret.txt
git -C "$secret_root" commit --quiet -m fixture
expect_failure \
    gitlab-pat \
    gitleaks git --redact=100 --no-banner --no-color --log-level error --verbose "$secret_root"

if [[ "${REQUIRE_BLOCKED_EGRESS:-0}" == "1" ]]; then
    network_output="$(bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>&1 || true)"
    if ! grep --extended-regexp --quiet 'Network is unreachable|Permission denied|Operation not permitted' <<<"$network_output"; then
        printf 'egress sentinel did not fail closed:\n%s\n' "$network_output" >&2
        exit 1
    fi
fi
