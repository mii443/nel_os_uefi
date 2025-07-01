#![no_main]
#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec};
use core::arch::asm;
use goblin::elf;
use uefi::{
    allocator::Allocator,
    boot::{MemoryType, ScopedProtocol},
    mem::memory_map::MemoryMap,
    prelude::*,
    println,
    proto::media::{
        file::{Directory, File, FileAttribute, FileInfo, FileMode},
        fs::SimpleFileSystem,
    },
    CStr16,
};

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn hlt_loop() -> ! {
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

fn get_fs() -> Directory {
    let mut fs: ScopedProtocol<SimpleFileSystem> =
        uefi::boot::get_image_file_system(uefi::boot::image_handle()).unwrap();

    fs.open_volume().unwrap()
}

fn read_kernel(name: &CStr16) -> Box<[u8]> {
    let mut root = get_fs();
    let kernel_file_info = root
        .open(name, FileMode::Read, FileAttribute::empty())
        .unwrap();
    let mut kernel_file = kernel_file_info.into_regular_file().unwrap();

    let file_size = kernel_file
        .get_boxed_info::<FileInfo>()
        .unwrap()
        .file_size();
    let mut buf = vec![0; file_size as usize];

    let read_size = kernel_file.read(&mut buf).unwrap();
    println!("kernel size: {}", read_size);

    buf.into_boxed_slice()
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    uefi::system::with_stdout(|stdout| stdout.clear().unwrap());

    println!("\nnel_os bootloader");

    let memory_map = uefi::boot::memory_map(MemoryType::LOADER_DATA).unwrap();
    println!("memory_map len: {}", memory_map.len());

    println!("Conventional memory:");
    for entry in memory_map.entries() {
        if entry.ty != MemoryType::CONVENTIONAL {
            continue;
        }
        println!("    Size: {:?}MiB", entry.page_count * 4 / 1024);
        println!("    PhysStart: {:?}", entry.phys_start);
    }

    let kernel = read_kernel(cstr16!("nel_os_kernel.elf"));

    let elf = elf::Elf::parse(&kernel).expect("Failed to parse kernel");

    println!("Entry point: {}", elf.entry);

    unsafe {
        let _ = uefi::boot::exit_boot_services(Some(MemoryType::LOADER_DATA));
    }

    hlt_loop();
}
