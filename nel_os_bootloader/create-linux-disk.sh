#!/usr/bin/env bash
set -euo pipefail

readonly OUTPUT_IMAGE="$(realpath -m -- "${1:?usage: create-linux-disk.sh OUTPUT SIZE_MIB BZIMAGE INITRAMFS}")"
readonly SIZE_MIB="${2:?usage: create-linux-disk.sh OUTPUT SIZE_MIB BZIMAGE INITRAMFS}"
readonly BZIMAGE="$(realpath -- "${3:?usage: create-linux-disk.sh OUTPUT SIZE_MIB BZIMAGE INITRAMFS}")"
readonly INITRAMFS="$(realpath -- "${4:?usage: create-linux-disk.sh OUTPUT SIZE_MIB BZIMAGE INITRAMFS}")"
readonly SECTOR_SIZE=512
readonly KERNEL_SECTOR=1

if [[ "${OUTPUT_IMAGE}" == "${BZIMAGE}" || "${OUTPUT_IMAGE}" == "${INITRAMFS}" ]]; then
    echo "Output image must differ from the Linux input files." >&2
    exit 2
fi

if [[ ! "${SIZE_MIB}" =~ ^[0-9]+$ ]] ||
    ((10#${SIZE_MIB} < 1 || 10#${SIZE_MIB} > 1048576)); then
    echo "Invalid Linux boot disk size in MiB: ${SIZE_MIB}" >&2
    exit 2
fi

readonly KERNEL_SIZE="$(stat -c %s -- "${BZIMAGE}")"
readonly KERNEL_SECTORS="$(((KERNEL_SIZE + SECTOR_SIZE - 1) / SECTOR_SIZE))"
readonly INITRAMFS_SECTOR="$((KERNEL_SECTOR + KERNEL_SECTORS))"
readonly INITRAMFS_SIZE="$(stat -c %s -- "${INITRAMFS}")"
readonly INITRAMFS_SECTORS="$(((INITRAMFS_SIZE + SECTOR_SIZE - 1) / SECTOR_SIZE))"
readonly REQUIRED_BYTES="$(((INITRAMFS_SECTOR + INITRAMFS_SECTORS) * SECTOR_SIZE))"
readonly IMAGE_BYTES="$((10#${SIZE_MIB} * 1024 * 1024))"

if ((REQUIRED_BYTES > IMAGE_BYTES)); then
    echo "Linux boot payload needs ${REQUIRED_BYTES} bytes, exceeding ${SIZE_MIB} MiB." >&2
    exit 2
fi

mkdir -p "$(dirname -- "${OUTPUT_IMAGE}")"
truncate -s "${IMAGE_BYTES}" "${OUTPUT_IMAGE}"
dd if=/dev/zero of="${OUTPUT_IMAGE}" bs="${SECTOR_SIZE}" count=1 conv=notrunc status=none
printf 'NELBOOT1%016x%016x%016x%016x' \
    "${KERNEL_SECTOR}" \
    "${KERNEL_SIZE}" \
    "${INITRAMFS_SECTOR}" \
    "${INITRAMFS_SIZE}" \
    | dd of="${OUTPUT_IMAGE}" bs=1 conv=notrunc status=none
dd if="${BZIMAGE}" of="${OUTPUT_IMAGE}" bs="${SECTOR_SIZE}" seek="${KERNEL_SECTOR}" conv=notrunc status=none
dd if="${INITRAMFS}" of="${OUTPUT_IMAGE}" bs="${SECTOR_SIZE}" seek="${INITRAMFS_SECTOR}" conv=notrunc status=none

echo "Created ${OUTPUT_IMAGE}: kernel=${KERNEL_SIZE} bytes initramfs=${INITRAMFS_SIZE} bytes"
