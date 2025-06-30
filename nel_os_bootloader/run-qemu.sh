#!/bin/sh

EFI_BINARY="$1"

cp "$EFI_BINARY" esp/efi/boot/bootx64.efi

qemu-system-x86_64 -enable-kvm \
    -m 4G \
    -nographic \
    -serial mon:stdio \
    -no-reboot \
    -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE.fd \
    -drive if=pflash,format=raw,readonly=on,file=OVMF_VARS.fd \
    -drive format=raw,file=fat:rw:esp
