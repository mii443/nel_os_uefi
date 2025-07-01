#![no_std]
#![no_main]

pub mod constant;
pub mod logging;
pub mod serial;

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::addr_of;

use crate::constant::{BANNER, KERNEL_STACK_SIZE, PKG_VERSION};

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
fn panic(_info: &PanicInfo) -> ! {
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
pub extern "sysv64" fn main() {
    println!("{} v{}", BANNER, PKG_VERSION);
    hlt_loop();
}
