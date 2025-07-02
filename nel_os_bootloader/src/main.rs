#![feature(vec_into_raw_parts)]
#![no_main]
#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec, vec::Vec};
use core::{arch::asm, slice};
use goblin::elf;
use nel_os_common::{gop, memory};
use uefi::{
    allocator::Allocator,
    boot::{AllocateType, MemoryType, ScopedProtocol},
    mem::memory_map::{MemoryMap, MemoryMapOwned},
    prelude::*,
    println,
    proto::{
        console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput, PixelFormat},
        media::{
            file::{Directory, File, FileAttribute, FileInfo, FileMode},
            fs::SimpleFileSystem,
        },
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

fn get_frame_buffer() -> gop::FrameBuffer {
    let gop_handle = uefi::boot::get_handle_for_protocol::<GraphicsOutput>().unwrap();
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle).unwrap();

    let info = gop.current_mode_info();
    let (width, height) = info.resolution();
    let frame_buffer = gop.frame_buffer().as_mut_ptr();
    let stride = info.stride();
    let pixel_format = info.pixel_format();

    gop::FrameBuffer {
        frame_buffer,
        width,
        height,
        stride,
        pixl_format: match pixel_format {
            PixelFormat::Rgb => gop::PixelFormat::Rgb,
            PixelFormat::Bgr => gop::PixelFormat::Bgr,
            format => panic!("Unsupported pixel_format: {:?}", format),
        },
    }
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    uefi::system::with_stdout(|stdout| stdout.clear().unwrap());

    println!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    let kernel = read_kernel(cstr16!("nel_os_kernel.elf"));

    let entry_point = load_elf(kernel);

    println!("Entry point: {:#x}", entry_point);

    let entry: extern "sysv64" fn(&nel_os_common::BootInfo) =
        unsafe { core::mem::transmute(entry_point) };

    let frame_buffer = get_frame_buffer();

    let size = uefi::boot::memory_map(MemoryType::LOADER_DATA)
        .unwrap()
        .len()
        + 8 * core::mem::size_of::<memory::Range>();
    let mut ranges: Vec<memory::Range> = Vec::with_capacity(size);

    println!("Usable memory table size: {}", size);

    let memory_map = unsafe { uefi::boot::exit_boot_services(Some(MemoryType::LOADER_DATA)) };

    memory_map
        .entries()
        .filter(|entry| {
            matches!(
                entry.ty,
                MemoryType::CONVENTIONAL
                    | MemoryType::BOOT_SERVICES_CODE
                    | MemoryType::BOOT_SERVICES_DATA
            )
        })
        .for_each(|entry| {
            ranges.push(memory::Range {
                start: entry.phys_start,
                end: entry.phys_start + entry.page_count * 4096,
            })
        });

    let usable_memory = {
        let (ptr, len, _) = ranges.into_raw_parts();
        memory::UsableMemory {
            ranges: ptr as *const memory::Range,
            len: len as u64,
        }
    };

    entry(&nel_os_common::BootInfo {
        usable_memory,
        frame_buffer,
    });

    hlt_loop();
}
