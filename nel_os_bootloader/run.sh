#!/bin/sh

cargo build --release --target x86_64-unknown-uefi

cp target/x86_64-unknown-uefi/release/nel_os_bootloader.efi esp/efi/boot/bootx64.efi

qemu-system-x86_64 -enable-kvm \
	-m 4G \
    -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE.fd \
    -drive if=pflash,format=raw,readonly=on,file=OVMF_VARS.fd \
    -drive format=raw,file=fat:rw:esp
