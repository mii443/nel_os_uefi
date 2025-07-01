#![no_std]
#![no_main]

pub mod constant;
pub mod logging;
pub mod serial;

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::addr_of;

use crate::constant::BANNER;

pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");
const STACK_SIZE: usize = 1024 * 1024;

#[repr(C, align(16))]
struct AlignedStack {
    stack: [u8; STACK_SIZE],
}

#[used]
static mut KERNEL_STACK: AlignedStack = AlignedStack {
    stack: [0; STACK_SIZE],
};

#[unsafe(no_mangle)]
pub extern "sysv64" fn asm_main() -> ! {
    unsafe {
        let stack_base = addr_of!(KERNEL_STACK.stack) as *const u8;
        let stack_top = stack_base.add(STACK_SIZE);

        asm!(
            "mov rsp, {stack_top}",
            "call {main}",
            "2:",
            "hlt",
            "jmp 2b",
            stack_top = in(reg) stack_top,
            main = sym main,
            options(noreturn)
        )
    }
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
