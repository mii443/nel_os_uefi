#!/usr/bin/env bash
set -euo pipefail

readonly PROJECT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCAL_CACHE_BASE="${NEL_OS_LOCAL_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME}/.cache}/nel_os_uefi}"
readonly CARGO_BIN="${CARGO:-${HOME}/.cargo/bin/cargo}"

if [[ ! -x "${CARGO_BIN}" ]]; then
    echo "cargo was not found at ${CARGO_BIN}. Install rustup before running nel_os." >&2
    exit 1
fi

for required_command in qemu-system-x86_64 xorriso mformat mmd mcopy; do
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        echo "${required_command} is required. On Ubuntu, install qemu-system-x86, ovmf, xorriso, and mtools." >&2
        exit 1
    fi
done

mkdir -p "${LOCAL_CACHE_BASE}/target"
export CARGO_TARGET_DIR="${NEL_OS_CARGO_TARGET_DIR:-${LOCAL_CACHE_BASE}/target}"

cd "${PROJECT_DIR}/nel_os_bootloader"
exec "${CARGO_BIN}" run --release "$@"
