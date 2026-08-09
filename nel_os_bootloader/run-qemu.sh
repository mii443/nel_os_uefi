#!/bin/bash -ex

EFI_BINARY="$1"

./clean.sh
./create-iso.sh "$EFI_BINARY"

QEMU_ACCEL_ARGS=()
if [[ -r /dev/kvm && -w /dev/kvm ]]; then
    QEMU_ACCEL_ARGS=(-enable-kvm -cpu host)
else
    QEMU_ACCEL_ARGS=(-machine accel=tcg -cpu max,+svm)
fi

run_qemu() {
    local debug_args=()
    if [[ -n "${NEL_OS_QEMU_GDB:-}" ]]; then
        debug_args=(-s)
    fi

    qemu-system-x86_64 "$@" \
        -m 512M \
        -serial mon:stdio \
        -nographic \
        -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE.fd \
        -drive if=pflash,format=raw,readonly=on,file=OVMF_VARS.fd \
        -cdrom nel_os.iso \
        -boot d \
        -smp 1 \
        "${debug_args[@]}" \
        --no-shutdown --no-reboot
}

if ! run_qemu "${QEMU_ACCEL_ARGS[@]}"; then
    if [[ "${QEMU_ACCEL_ARGS[*]}" == *"-enable-kvm"* ]]; then
        run_qemu -machine accel=tcg -cpu max,+svm
    else
        exit 1
    fi
fi
