#!/bin/sh
cargo b -r
sudo mount /dev/loop0 /mnt
sudo cp ./target/x86_64-unknown-uefi/release/nel_os_bootloader.efi /mnt/EFI/BOOT/BOOTX64.EFI
sudo sync
sudo umount /mnt
