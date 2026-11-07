#!/usr/bin/env bash
# Cargo subcommand: build/check/publish the Alixer RPMs.
# Install with: ln -s "$(git rev-parse --show-toplevel)/packaging/rpm/cargo-rpm.sh" ~/.cargo/bin/cargo-rpm
set -euo pipefail
here="$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")" && pwd)"
# Cargo passes the invoked subcommand name as $1: `cargo rpm publish` runs
# `cargo-rpm rpm publish`.
[[ "${1:-}" == rpm ]] && shift
subcommand="${1:-build}"; shift || true
case "$subcommand" in
    build) bash "$here/build.sh" "$@" ;;
    check) bash "$here/check.sh" "$@" ;;
    publish)
        if [[ ! -f "$here/publish.sh" ]]; then
            echo "Publishing requires a local packaging/rpm/publish.sh script (not tracked in Git)." >&2
            exit 1
        fi
        bash "$here/publish.sh" "$@"
        ;;
    *) echo "Usage: cargo rpm [build|check|publish] [args...]" >&2; exit 2 ;;
esac
