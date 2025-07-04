#!/bin/sh -ex

EFI_BINARY="$1"

./clean.sh
./create-iso.sh "$EFI_BINARY"

qemu-system-x86_64 -enable-kvm \
    -m 4G \
    -serial mon:stdio \
    -no-reboot \
    -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE.fd \
    -drive if=pflash,format=raw,readonly=on,file=OVMF_VARS.fd \
	-cdrom nel_os.iso \
	-boot d \
	-cpu host \
	-s
