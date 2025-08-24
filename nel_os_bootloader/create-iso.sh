#!/bin/bash -ex

EFI_BINARY="$1"

cd ../nel_os_kernel
if [[ "$EFI_BINARY" == "target/x86_64-unknown-uefi/release/"* ]]; then
	cargo build --release -q
elif [[ "$EFI_BINARY" == "target/x86_64-unknown-uefi/debug/"* ]]; then
	cargo build -q
else
	echo "Error: EFI binary path must contain either '/target/x86_64-unknown-uefi/release/' or '/target/x86_64-unknown-uefi/debug/'"
	exit 1
fi
cd ../nel_os_bootloader

dd if=/dev/zero of=fat.img bs=1k count=32768
mformat -i fat.img -C -h 16 -t 128 -s 32 ::
mmd -i fat.img ::/EFI
mmd -i fat.img ::/EFI/BOOT
mcopy -i fat.img "$EFI_BINARY" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i fat.img ../nel_os_kernel/target/x86_64-nel_os/release/nel_os_kernel.elf ::/nel_os_kernel.elf
mcopy -i fat.img bzImage ::/bzImage
mcopy -i fat.img rootfs-n.cpio.gz ::/rootfs-n.cpio.gz

mkdir iso
cp fat.img iso
xorriso -as mkisofs -R -f -e fat.img -no-emul-boot -o nel_os.iso iso
