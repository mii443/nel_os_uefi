#!/usr/bin/env bash
set -euo pipefail

readonly OUTPUT_IMAGE="$(realpath -m -- "${1:?usage: create-linux-disk.sh OUTPUT SIZE_MIB BZIMAGE INITRAMFS}")"
readonly SIZE_MIB="${2:?usage: create-linux-disk.sh OUTPUT SIZE_MIB BZIMAGE INITRAMFS}"
readonly BZIMAGE="$(realpath -- "${3:?usage: create-linux-disk.sh OUTPUT SIZE_MIB BZIMAGE INITRAMFS}")"
readonly INITRAMFS="$(realpath -- "${4:?usage: create-linux-disk.sh OUTPUT SIZE_MIB BZIMAGE INITRAMFS}")"
readonly SYSTEMD_BOOT="${NEL_OS_SYSTEMD_BOOT:-/usr/lib/systemd/boot/efi/systemd-bootx64.efi}"
readonly KERNEL_OPTIONS="${NEL_OS_LINUX_KERNEL_OPTIONS:-console=ttyS0 quiet loglevel=4 nokaslr pci=conf1 acpi=off noapic nolapic}"
readonly ESP_START_SECTOR=2048
readonly SECTOR_SIZE=512

if [[ "${OUTPUT_IMAGE}" == "${BZIMAGE}" || "${OUTPUT_IMAGE}" == "${INITRAMFS}" ]]; then
    echo "Output image must differ from the Linux input files." >&2
    exit 2
fi
if [[ ! -f "${SYSTEMD_BOOT}" ]]; then
    echo "systemd-boot EFI binary was not found: ${SYSTEMD_BOOT}" >&2
    exit 2
fi
if [[ ! "${SIZE_MIB}" =~ ^[0-9]+$ ]] ||
    ((10#${SIZE_MIB} < 32 || 10#${SIZE_MIB} > 1048576)); then
    echo "Invalid Linux VM disk size in MiB: ${SIZE_MIB} (minimum 32)" >&2
    exit 2
fi

readonly IMAGE_BYTES="$((10#${SIZE_MIB} * 1024 * 1024))"
readonly ESP_OFFSET="$((ESP_START_SECTOR * SECTOR_SIZE))"
CONFIG_DIR="$(mktemp -d)"
readonly CONFIG_DIR
trap 'rm -rf -- "${CONFIG_DIR}"' EXIT

mkdir -p "$(dirname -- "${OUTPUT_IMAGE}")"
truncate -s "${IMAGE_BYTES}" "${OUTPUT_IMAGE}"
sgdisk --zap-all "${OUTPUT_IMAGE}" >/dev/null
sgdisk --new=1:${ESP_START_SECTOR}:0 --typecode=1:ef00 --change-name=1:NEL-ESP \
    "${OUTPUT_IMAGE}" >/dev/null

readonly MTOOLS_IMAGE="${OUTPUT_IMAGE}@@${ESP_OFFSET}"
mformat -i "${MTOOLS_IMAGE}" -F -v NEL-ESP ::
mmd -i "${MTOOLS_IMAGE}" ::/EFI ::/EFI/BOOT ::/EFI/Linux
mmd -i "${MTOOLS_IMAGE}" ::/loader ::/loader/entries
mcopy -i "${MTOOLS_IMAGE}" "${SYSTEMD_BOOT}" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "${MTOOLS_IMAGE}" "${BZIMAGE}" ::/EFI/Linux/nel-linux.efi
mcopy -i "${MTOOLS_IMAGE}" "${INITRAMFS}" ::/initrd.img

printf 'default nel-linux.conf\ntimeout 0\nconsole-mode keep\n' >"${CONFIG_DIR}/loader.conf"
printf '%s\n' \
    'title nel Linux' \
    'linux /EFI/Linux/nel-linux.efi' \
    'initrd /initrd.img' \
    "options ${KERNEL_OPTIONS}" \
    >"${CONFIG_DIR}/nel-linux.conf"
mcopy -i "${MTOOLS_IMAGE}" "${CONFIG_DIR}/loader.conf" ::/loader/loader.conf
mcopy -i "${MTOOLS_IMAGE}" "${CONFIG_DIR}/nel-linux.conf" ::/loader/entries/nel-linux.conf

echo "Created UEFI VM disk ${OUTPUT_IMAGE}: ${SIZE_MIB} MiB"
