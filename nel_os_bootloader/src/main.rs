#![no_main]
#![no_std]

extern crate alloc;

use core::arch::asm;
use uefi::{
    allocator::Allocator,
    boot::{MemoryType, ScopedProtocol},
    mem::memory_map::MemoryMap,
    prelude::*,
    println,
    proto::media::{file::Directory, fs::SimpleFileSystem},
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

    let mut root = get_fs();

    println!("Root directory entries:");
    while let Ok(Some(file_info)) = root.read_entry_boxed() {
        if file_info.is_directory() {
            println!("Directory: {}", file_info.file_name());
        } else {
            println!("File: {}", file_info.file_name());
        }
    }

    hlt_loop();
}
