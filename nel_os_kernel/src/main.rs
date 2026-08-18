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
mod management;
pub mod memory;
pub mod network;
pub mod platform;
pub mod serial;
mod serial_console;
pub mod time;
mod vm_control;
pub mod vmm;

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::addr_of;

use ::acpi::AcpiTables;
use spin::Once;
use x86_64::{VirtAddr, registers::control::Cr3, structures::paging::OffsetPageTable};

use crate::{
    acpi::KernelAcpiHandler,
    constant::{KERNEL_STACK_SIZE, PKG_VERSION},
    graphics::{FRAME_BUFFER, FrameBuffer},
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

#[unsafe(no_mangle)]
pub extern "sysv64" fn asm_main(boot_info: &nel_os_common::BootInfo) -> ! {
    unsafe {
        let stack_base = addr_of!(KERNEL_STACK.stack) as *const u8;
        let stack_top = stack_base.add(KERNEL_STACK_SIZE);

        asm!(
            "mov rsp, {stack_top}",
            "call {main}",
            stack_top = in(reg) stack_top,
            main = sym main,
            in("rdi") boot_info,
            clobber_abi("sysv64"),
            options(noreturn),
        )
    }
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
pub extern "sysv64" fn main(boot_info: &nel_os_common::BootInfo) -> ! {
    let boot_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    serial::disable_screen_output();

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
        info!("Local APIC initialized",);

        x86_64::instructions::interrupts::enable();

        info!("Interrupts enabled");
    }

    BZIMAGE_ADDR.call_once(|| boot_info.bzimage_addr);
    BZIMAGE_SIZE.call_once(|| boot_info.bzimage_size);
    ROOTFS_ADDR.call_once(|| boot_info.rootfs_addr);
    ROOTFS_SIZE.call_once(|| boot_info.rootfs_size);

    // The physical NIC is owned by the outer kernel. It is initialized before
    // guest RAM is created, and is never mapped into EPT/NPT or represented by
    // the guest's deliberately empty PCI model.
    let network_device = match network::VirtioNet::probe(&mut bitmap_table) {
        Ok(device) => Some(device),
        Err(error) => {
            error!("Hypervisor network unavailable: {}", error);
            warn!("TCP management is disabled; the local serial shell remains available");
            None
        }
    };

    if network_device.is_some() {
        info!(
            "{} Linux VM slots are stopped; management shell will listen on TCP port {} after DHCP",
            vmm::MAX_VMS,
            network::CONTROL_PORT,
        );
    } else {
        info!(
            "{} Linux VM slots are stopped; use the local serial management shell",
            vmm::MAX_VMS
        );
    }

    vm_control::VmController::new(network_device, usable_frame, boot_tsc).run(&mut bitmap_table);
}
