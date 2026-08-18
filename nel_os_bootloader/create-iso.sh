#!/usr/bin/env bash
set -euxo pipefail

readonly SOURCE_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly KERNEL_DIR="$(cd -- "${SOURCE_DIR}/../nel_os_kernel" && pwd)"
readonly EFI_BINARY="$(realpath -- "$1")"
readonly RUNTIME_DIR="$(realpath -m -- "$2")"
readonly FAT_IMAGE="${RUNTIME_DIR}/fat.img"
readonly ISO_ROOT="${RUNTIME_DIR}/iso"
readonly ISO_IMAGE="${RUNTIME_DIR}/nel_os.iso"
KERNEL_PROFILE=""
KERNEL_BUILD_ARGS=()

case "${EFI_BINARY}" in
    */release/*)
        KERNEL_PROFILE="release"
        KERNEL_BUILD_ARGS=(--release)
        ;;
    */debug/*)
        KERNEL_PROFILE="debug"
        ;;
    *)
        echo "Error: EFI binary path must contain a release or debug profile directory." >&2
        exit 1
        ;;
esac

mkdir -p "${RUNTIME_DIR}"
"${SOURCE_DIR}/clean.sh" "${RUNTIME_DIR}"

(
    cd "${KERNEL_DIR}"
    cargo build "${KERNEL_BUILD_ARGS[@]}" -Zjson-target-spec -q
)

if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    if [[ "${CARGO_TARGET_DIR}" = /* ]]; then
        KERNEL_TARGET_DIR="${CARGO_TARGET_DIR}"
    else
        KERNEL_TARGET_DIR="${KERNEL_DIR}/${CARGO_TARGET_DIR}"
    fi
else
    KERNEL_TARGET_DIR="${KERNEL_DIR}/target"
fi
readonly KERNEL_BINARY="${KERNEL_TARGET_DIR}/x86_64-nel_os/${KERNEL_PROFILE}/nel_os_kernel.elf"

dd if=/dev/zero of="${FAT_IMAGE}" bs=1k count=32768
mformat -i "${FAT_IMAGE}" -C -h 16 -t 128 -s 32 ::
mmd -i "${FAT_IMAGE}" ::/EFI
mmd -i "${FAT_IMAGE}" ::/EFI/BOOT
mcopy -i "${FAT_IMAGE}" "${EFI_BINARY}" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "${FAT_IMAGE}" "${KERNEL_BINARY}" ::/nel_os_kernel.elf
mcopy -i "${FAT_IMAGE}" "${SOURCE_DIR}/bzImage" ::/bzImage
mcopy -i "${FAT_IMAGE}" "${SOURCE_DIR}/rootfs-n.cpio.gz" ::/rootfs-n.cpio.gz

mkdir "${ISO_ROOT}"
cp "${FAT_IMAGE}" "${ISO_ROOT}/fat.img"
xorriso -as mkisofs -R -f -e fat.img -no-emul-boot -o "${ISO_IMAGE}" "${ISO_ROOT}"
