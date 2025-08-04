#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(allocator_api)]

extern crate alloc;

pub mod acpi;
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
use x86_64::{registers::control::Cr3, structures::paging::OffsetPageTable, VirtAddr};

use crate::{
    acpi::KernelAcpiHandler,
    constant::{KERNEL_STACK_SIZE, PKG_VERSION},
    graphics::{FrameBuffer, FRAME_BUFFER},
    interrupt::apic,
    memory::{allocator, memory::BitmapMemoryTable, paging},
};

#[repr(C, align(16))]
struct AlignedStack {
    stack: [u8; KERNEL_STACK_SIZE],
}

#[used]
static mut KERNEL_STACK: AlignedStack = AlignedStack {
    stack: [0; KERNEL_STACK_SIZE],
};

#[unsafe(no_mangle)]
pub extern "sysv64" fn asm_main() -> ! {
    unsafe {
        let stack_base = addr_of!(KERNEL_STACK.stack) as *const u8;
        let stack_top = stack_base.add(KERNEL_STACK_SIZE);

        asm!(
            "mov rsp, {stack_top}",
            "call {main}",
            stack_top = in(reg) stack_top,
            main = sym main
        )
    }

    hlt_loop();
}

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
    let mut count = 0;
    for range in ranges {
        count += range.end - range.start;
    }
    info!("Usable memory: {}MiB", count / 1024 / 1024);

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

    let frame_buffer = FrameBuffer::from_raw_buffer(&boot_info.frame_buffer, (64, 64, 64));
    frame_buffer.clear();

    FRAME_BUFFER.lock().replace(frame_buffer);

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

    info!("RSDP: {:#x}", boot_info.rsdp);

    let acpi_tables =
        unsafe { AcpiTables::from_rsdp(KernelAcpiHandler, boot_info.rsdp as usize) }.unwrap();
    let platform_info = acpi_tables.platform_info().unwrap();

    apic::init_local_apic(platform_info);
    info!("Local APIC initialized",);

    x86_64::instructions::interrupts::enable();

    info!("Interrupts enabled");

    let mut vcpu = vmm::get_vcpu().unwrap();
    vcpu.run();

    hlt_loop();
}
