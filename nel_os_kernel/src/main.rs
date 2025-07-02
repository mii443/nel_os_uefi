#![no_std]
#![no_main]

extern crate alloc;

pub mod allocator;
pub mod constant;
pub mod logging;
pub mod memory;
pub mod paging;
pub mod serial;

use alloc::vec;

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::addr_of;

use x86_64::{structures::paging::OffsetPageTable, VirtAddr};

use crate::{
    constant::{BANNER, KERNEL_STACK_SIZE, PKG_VERSION},
    memory::BitmapMemoryTable,
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
    println!("{} v{}", BANNER, PKG_VERSION);

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

    let mut test_vec = vec![1, 2, 3, 4, 9];
    test_vec.push(10);
    info!("Vector test: {:?}", test_vec);

    hlt_loop();
}
