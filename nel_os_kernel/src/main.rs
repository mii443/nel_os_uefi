#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(allocator_api)]
#![feature(avx512_target_feature)]
#![feature(naked_functions)]
#![feature(optimize_attribute)]
#![feature(stdarch_x86_avx512)]

extern crate alloc;

pub mod acpi;
pub mod avx;
pub mod constant;
pub mod cpuid;
pub mod graphics;
pub mod interrupt;
pub mod logging;
pub mod memory;
pub mod platform;
pub mod serial;
pub mod time;
pub mod vmm;

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::addr_of;

use ::acpi::AcpiTables;
use spin::Once;
use x86_64::{
    instructions::hlt,
    registers::control::{Cr3, Cr4Flags},
    structures::paging::OffsetPageTable,
    VirtAddr,
};

use crate::{
    acpi::KernelAcpiHandler,
    avx::avx_benchmark,
    constant::{KERNEL_STACK_SIZE, PKG_VERSION},
    graphics::{FrameBuffer, FRAME_BUFFER},
    interrupt::apic,
    memory::{allocator, bitmap::BitmapMemoryTable, paging},
};

pub static BZIMAGE_ADDR: Once<u64> = Once::new();
pub static BZIMAGE_SIZE: Once<u64> = Once::new();
pub static ROOTFS_ADDR: Once<u64> = Once::new();
pub static ROOTFS_SIZE: Once<u64> = Once::new();

#[repr(C, align(16))]
struct AlignedStack {
    stack: [u8; KERNEL_STACK_SIZE],
}

#[used]
static mut KERNEL_STACK: AlignedStack = AlignedStack {
    stack: [0; KERNEL_STACK_SIZE],
};

// Entry point defined in assembly to avoid stack alignment issues
// RSP must be 16-byte aligned BEFORE the call instruction per x86-64 ABI
// We receive boot_info pointer in RDI from bootloader, must preserve it
core::arch::global_asm!(
    ".global asm_main",
    ".type asm_main, @function",
    "asm_main:",
    "   lea rax, [rip + {kernel_stack}]",
    "   add rax, {stack_size}",
    "   and rax, -16",
    "   mov rsp, rax",
    // RDI already contains boot_info pointer from bootloader, don't touch it
    "   call {main}",
    "1: hlt",
    "   jmp 1b",
    kernel_stack = sym KERNEL_STACK,
    stack_size = const KERNEL_STACK_SIZE,
    main = sym main,
);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    hlt_loop();
}

#[inline]
fn hlt_loop() -> ! {
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

#[unsafe(no_mangle)]
pub extern "sysv64" fn main(boot_info: &nel_os_common::BootInfo) {
    serial::disable_screen_output();

    // Debug: check boot_info pointer
    info!("boot_info ptr: {:p}", boot_info);

    interrupt::gdt::init();
    interrupt::idt::init_idt();

    let virt = VirtAddr::new(
        x86_64::registers::control::Cr3::read()
            .0
            .start_address()
            .as_u64(),
    );
    let phys = paging::translate_addr(virt);
    info!("Level 4 page table: {:?} -> {:?}", virt, phys);

    let ranges = boot_info.usable_memory.ranges();
    info!("Memory ranges count: {}", ranges.len());
    let mut count = 0;
    let mut max_range = 0;
    for range in ranges {
        count += range.end - range.start;
        max_range = max_range.max(range.end);
    }
    info!("Usable memory: {}MiB", count / 1024 / 1024);
    memory::bitmap::MAX_MEMORY.call_once(|| max_range as usize * 2);

    let mut bitmap_table = BitmapMemoryTable::init(&boot_info.usable_memory);
    info!(
        "Memory bitmap initialized: {} -> {}",
        bitmap_table.start, bitmap_table.end
    );

    let mut usable_frame = 0;
    for i in bitmap_table.start..bitmap_table.end {
        if bitmap_table.get_bit(i) {
            usable_frame += 1;
        }
    }

    info!("Usable memory in bitmap: {}MiB", usable_frame * 4 / 1024);

    let mut mapper = {
        let lv4_table_ptr = paging::init_page_table(&mut bitmap_table);
        let lv4_table = unsafe { &mut *lv4_table_ptr };
        unsafe { OffsetPageTable::new(lv4_table, VirtAddr::new(0x0)) }
    };

    info!("Page table initialized");

    allocator::init_heap(&mut mapper, &mut bitmap_table).unwrap();

    if boot_info.frame_buffer.is_some() {
        let frame_buffer =
            FrameBuffer::from_raw_buffer(boot_info.frame_buffer.as_ref().unwrap(), (64, 64, 64));
        frame_buffer.clear();

        FRAME_BUFFER.lock().replace(frame_buffer);
    } else {
        error!("No frame buffer found");
    }

    println!("");
    info!("Kernel initialized successfully");

    info!("Kernel version: {}", PKG_VERSION);
    info!(
        "Level 4 page table at {:#x}",
        Cr3::read().0.start_address().as_u64()
    );
    info!(
        "Memory bitmap: {} -> {}",
        bitmap_table.start, bitmap_table.end
    );
    info!("CPU: {} {}", cpuid::get_vendor_id(), cpuid::get_brand());
    info!(
        "Usable memory: {}MiB ({:.1}GiB)",
        usable_frame * 4 / 1024,
        usable_frame as f64 * 4. / 1024. / 1024.
    );

    if let Some(rsdp) = boot_info.rsdp {
        info!("RSDP: {:#x}", rsdp);

        let acpi_tables =
            unsafe { AcpiTables::from_rsdp(KernelAcpiHandler, rsdp as usize) }.unwrap();
        let platform_info = acpi_tables.platform_info().unwrap();

        apic::init_local_apic(platform_info);
        info!("Local APIC initialized");

        // Verify stack alignment before enabling interrupts
        let rsp: usize;
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) rsp);
        }
        info!("RSP before sti: {:#x} (aligned: {})", rsp, rsp % 16 == 0);

        info!("About to enable interrupts (sti)...");
        x86_64::instructions::interrupts::enable();
        info!("STI executed, interrupts enabled");

        // Small delay to see if we get here
        for _ in 0..1000000 {
            core::hint::spin_loop();
        }
        info!("Interrupts working correctly");
    }

    // init AVX
    unsafe {
        info!("Enabling AVX support...");
        let mut cr4 = x86_64::registers::control::Cr4::read();
        cr4 = cr4.union(Cr4Flags::OSXSAVE);
        info!(
            "CR4 before: {:#x}",
            x86_64::registers::control::Cr4::read().bits()
        );
        x86_64::registers::control::Cr4::write(cr4);

        let xcr0: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 5) | (1 << 6) | (1 << 7);
        info!("Setting XCR0 to {:#x}", xcr0);
        asm!(
            "xsetbv",
            in("ecx") 0,
            in("eax") (xcr0 & 0xFFFFFFFF) as u32,
            in("edx") (xcr0 >> 32) as u32,
        );
    }

    loop {
        avx_benchmark(true);
        avx_benchmark(false);
    }

    loop {
        hlt();
    }

    BZIMAGE_ADDR.call_once(|| boot_info.bzimage_addr);
    BZIMAGE_SIZE.call_once(|| boot_info.bzimage_size);
    ROOTFS_ADDR.call_once(|| boot_info.rootfs_addr);
    ROOTFS_SIZE.call_once(|| boot_info.rootfs_size);

    let mut vcpu = vmm::get_vcpu(&mut bitmap_table).unwrap();

    info!("Running guest VM...");
    loop {
        let result = vcpu.run(&mut bitmap_table);
        if let Err(e) = result {
            error!("VCPU run failed: {}", e);
            break;
        }
    }

    hlt_loop();
}
