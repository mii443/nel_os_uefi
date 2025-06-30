#!/bin/sh

EFI_BINARY="$1"

dd if=/dev/zero of=fat.img bs=1k count=1440
mformat -i fat.img -f 1440 ::
mmd -i fat.img ::/EFI
mmd -i fat.img ::/EFI/BOOT
mcopy -i fat.img "$EFI_BINARY" ::/EFI/BOOT/BOOTX64.EFI

mkdir iso
cp fat.img iso
xorriso -as mkisofs -R -f -e fat.img -no-emul-boot -o nel_os.iso iso
