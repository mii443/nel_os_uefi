#![no_main]
#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec};
use core::{arch::asm, slice};
use goblin::elf::{self, Elf};
use uefi::{
    allocator::Allocator,
    boot::{AllocateType, MemoryType, ScopedProtocol},
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

fn load_elf(bin: Box<[u8]>) -> u64 {
    let elf = elf::Elf::parse(&bin).expect("Failed to parse elf");
    let mut dest_start = u64::MAX;
    let mut dest_end = 0u64;

    elf.program_headers
        .iter()
        .filter(|header| header.p_type == elf::program_header::PT_LOAD)
        .for_each(|header| {
            dest_start = dest_start.min(header.p_vaddr);
            dest_end = dest_end.max(header.p_vaddr + header.p_memsz);
        });

    uefi::boot::allocate_pages(
        AllocateType::Address(dest_start),
        MemoryType::LOADER_DATA,
        dest_end
            .checked_sub(dest_start)
            .and_then(|size| size.checked_add(4095))
            .map(|size| size / 4096)
            .unwrap_or(0) as usize,
    )
    .expect("Failed to allocate pages");

    elf.program_headers
        .iter()
        .filter(|header| header.p_type == elf::program_header::PT_LOAD)
        .for_each(|header| {
            let dest = unsafe {
                slice::from_raw_parts_mut(header.p_vaddr as *mut u8, header.p_memsz as usize)
            };

            let file_size = header.p_filesz as usize;
            let offset = header.p_offset as usize;

            dest[..file_size].copy_from_slice(&bin[offset..offset + file_size]);
            dest[file_size..].fill(0);
        });

    elf.entry
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

    let entry_point = load_elf(kernel);

    println!("Entry point: {:#x}", entry_point);

    let entry: extern "sysv64" fn() = unsafe { core::mem::transmute(entry_point) };

    unsafe {
        let _ = uefi::boot::exit_boot_services(Some(MemoryType::LOADER_DATA));
    }

    entry();

    hlt_loop();
}
