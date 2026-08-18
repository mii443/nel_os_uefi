#!/usr/bin/env bash
set -euxo pipefail

readonly RUNTIME_DIR="${1:?usage: clean.sh RUNTIME_DIR}"

rm -rf -- "${RUNTIME_DIR}/iso"
rm -f -- "${RUNTIME_DIR}/fat.img" "${RUNTIME_DIR}/nel_os.iso"
