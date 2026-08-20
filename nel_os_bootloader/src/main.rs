#![no_main]
#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec, vec::Vec};
use core::slice;
use goblin::elf;
use nel_os_common::{gop, memory};
use uefi::{
    CStr16,
    allocator::Allocator,
    boot::{AllocateType, MemoryType, ScopedProtocol},
    mem::memory_map::MemoryMap,
    prelude::*,
    println,
    proto::{
        console::gop::{GraphicsOutput, PixelFormat},
        media::{
            file::{Directory, File, FileAttribute, FileInfo, FileMode},
            fs::SimpleFileSystem,
        },
    },
};

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn get_fs() -> Directory {
    let mut fs: ScopedProtocol<SimpleFileSystem> =
        uefi::boot::get_image_file_system(uefi::boot::image_handle()).unwrap();

    fs.open_volume().unwrap()
}

fn read_file(name: &CStr16) -> Box<[u8]> {
    let mut root = get_fs();
    let file_info = root
        .open(name, FileMode::Read, FileAttribute::empty())
        .unwrap();
    let mut file = file_info.into_regular_file().unwrap();

    let file_size = usize::try_from(file.get_boxed_info::<FileInfo>().unwrap().file_size())
        .expect("File is too large for this platform");
    let mut buf = vec![0; file_size];
    let mut read_size = 0;
    while read_size < buf.len() {
        let count = file
            .read(&mut buf[read_size..])
            .expect("Failed to read file");
        assert!(count != 0, "File ended before its reported size");
        read_size = read_size.checked_add(count).expect("File size overflow");
    }
    println!("file {} size: {}", name, read_size);

    buf.into_boxed_slice()
}

fn load_file_to_loader_data(name: &CStr16) -> (u64, u64) {
    let mut root = get_fs();
    let file_info = root
        .open(name, FileMode::Read, FileAttribute::empty())
        .expect("Failed to open file");
    let mut file = file_info
        .into_regular_file()
        .expect("Failed to convert to regular file");
    let file_size = file
        .get_boxed_info::<FileInfo>()
        .expect("Failed to get file info")
        .file_size();
    assert!(file_size != 0, "Guest firmware is empty");
    let file_size_usize = usize::try_from(file_size).expect("Guest firmware is too large");
    let page_ptr = uefi::boot::allocate_pages(
        AllocateType::AnyPages,
        MemoryType::LOADER_DATA,
        file_size.div_ceil(4096) as usize,
    )
    .expect("Failed to allocate pages")
    .as_ptr();
    let buffer = unsafe { slice::from_raw_parts_mut(page_ptr, file_size_usize) };
    let mut read_size = 0;
    while read_size < buffer.len() {
        let count = file
            .read(&mut buffer[read_size..])
            .expect("Failed to read guest firmware");
        assert!(count != 0, "Guest firmware ended before its reported size");
        read_size = read_size
            .checked_add(count)
            .expect("Guest firmware size overflow");
    }
    println!("file {} size: {}", name, read_size);
    (page_ptr as u64, file_size)
}

fn load_elf(bin: Box<[u8]>) -> u64 {
    let elf = elf::Elf::parse(&bin).expect("Failed to parse elf");
    assert!(elf.is_64, "Kernel must be a 64-bit ELF image");
    assert!(elf.little_endian, "Kernel ELF must be little-endian");
    assert!(
        elf.header.e_type == elf::header::ET_EXEC,
        "Kernel ELF must be an executable image"
    );
    assert!(
        elf.header.e_machine == elf::header::EM_X86_64,
        "Kernel ELF is not x86-64"
    );
    let mut dest_start = u64::MAX;
    let mut dest_end = 0u64;
    let mut load_segments = 0usize;
    let mut entry_is_executable = false;

    for (index, header) in elf.program_headers.iter().enumerate() {
        if header.p_type != elf::program_header::PT_LOAD {
            continue;
        }
        load_segments += 1;
        assert!(
            header.p_filesz <= header.p_memsz,
            "ELF segment file size exceeds memory size"
        );
        let file_end = header
            .p_offset
            .checked_add(header.p_filesz)
            .expect("ELF segment file range overflow");
        assert!(
            file_end <= bin.len() as u64,
            "ELF segment exceeds the kernel file"
        );
        let memory_end = header
            .p_vaddr
            .checked_add(header.p_memsz)
            .expect("ELF segment memory range overflow");
        assert!(
            memory_end <= 1u64 << 47,
            "Kernel ELF is outside physical address space"
        );
        assert!(
            header.p_align == 0 || header.p_align.is_power_of_two(),
            "Invalid ELF alignment"
        );
        if header.p_align > 1 {
            assert!(
                header.p_vaddr % header.p_align == header.p_offset % header.p_align,
                "ELF segment file and memory alignment disagree"
            );
        }
        assert!(
            header.p_flags & (elf::program_header::PF_W | elf::program_header::PF_X)
                != elf::program_header::PF_W | elf::program_header::PF_X,
            "Writable and executable ELF segments are rejected"
        );
        for previous in elf.program_headers[..index]
            .iter()
            .filter(|previous| previous.p_type == elf::program_header::PT_LOAD)
        {
            let previous_end = previous
                .p_vaddr
                .checked_add(previous.p_memsz)
                .expect("ELF segment memory range overflow");
            assert!(
                header.p_memsz == 0
                    || previous.p_memsz == 0
                    || memory_end <= previous.p_vaddr
                    || previous_end <= header.p_vaddr,
                "ELF load segments overlap"
            );
        }
        if header.p_flags & elf::program_header::PF_X != 0
            && (header.p_vaddr..memory_end).contains(&elf.entry)
        {
            entry_is_executable = true;
        }
        dest_start = dest_start.min(header.p_vaddr);
        dest_end = dest_end.max(memory_end);
    }
    assert!(
        load_segments != 0,
        "Kernel ELF contains no loadable segments"
    );
    assert!(
        entry_is_executable,
        "Kernel entry point is not in an executable segment"
    );

    let allocation_start = dest_start & !4095;
    assert!(
        allocation_start != 0,
        "Kernel ELF may not be loaded at physical address zero"
    );
    let allocation_end = dest_end
        .checked_add(4095)
        .map(|end| end & !4095)
        .expect("Kernel allocation range overflow");
    let page_count = allocation_end
        .checked_sub(allocation_start)
        .map(|size| size / 4096)
        .and_then(|pages| usize::try_from(pages).ok())
        .filter(|pages| *pages != 0)
        .expect("Kernel allocation range is invalid");

    uefi::boot::allocate_pages(
        AllocateType::Address(allocation_start),
        MemoryType::LOADER_DATA,
        page_count,
    )
    .expect("Failed to allocate pages");

    unsafe {
        core::ptr::write_bytes(allocation_start as *mut u8, 0, page_count * 4096);
    }

    for header in elf
        .program_headers
        .iter()
        .filter(|header| header.p_type == elf::program_header::PT_LOAD)
    {
        let memory_size = usize::try_from(header.p_memsz).expect("ELF segment is too large");
        let file_size = usize::try_from(header.p_filesz).expect("ELF segment is too large");
        let offset = usize::try_from(header.p_offset).expect("ELF file offset is too large");
        let dest = unsafe { slice::from_raw_parts_mut(header.p_vaddr as *mut u8, memory_size) };
        dest[..file_size].copy_from_slice(&bin[offset..offset + file_size]);
    }

    elf.entry
}

fn get_frame_buffer() -> Option<gop::FrameBuffer> {
    let gop_handle = if let Ok(gop_handle) = uefi::boot::get_handle_for_protocol::<GraphicsOutput>()
    {
        gop_handle
    } else {
        println!("GraphicsOutput protocol not found");
        return None;
    };
    let mut gop = if let Ok(gop) = boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle) {
        gop
    } else {
        println!("Failed to open GraphicsOutput protocol");
        return None;
    };

    let info = gop.current_mode_info();
    let (width, height) = info.resolution();
    let frame_buffer = gop.frame_buffer().as_mut_ptr();
    let stride = info.stride();
    let pixel_format = info.pixel_format();

    Some(gop::FrameBuffer {
        frame_buffer,
        width,
        height,
        stride,
        pixl_format: match pixel_format {
            PixelFormat::Rgb => gop::PixelFormat::Rgb,
            PixelFormat::Bgr => gop::PixelFormat::Bgr,
            format => panic!("Unsupported pixel_format: {:?}", format),
        },
    })
}

fn get_rsdp() -> Option<u64> {
    uefi::system::with_config_table(move |c| {
        c.iter()
            .find(|config| config.guid == uefi::table::cfg::ACPI_GUID)
            .map(|config| config.address as u64)
            .or(None)
    })
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    uefi::system::with_stdout(|stdout| stdout.clear().unwrap());

    println!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    let kernel = read_file(cstr16!("nel_os_kernel.elf"));
    let (guest_firmware_addr, guest_firmware_size) =
        load_file_to_loader_data(cstr16!("guest-firmware.fd"));

    let entry_point = load_elf(kernel);

    println!("Entry point: {:#x}", entry_point);

    let entry: extern "sysv64" fn(&nel_os_common::BootInfo) -> ! =
        unsafe { core::mem::transmute(entry_point) };

    let frame_buffer = get_frame_buffer();

    let rsdp = get_rsdp();

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
        rsdp,
        guest_firmware_addr,
        guest_firmware_size,
    });
}
