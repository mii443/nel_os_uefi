use core::arch::asm;

use crate::info;

pub mod vcpu;
pub mod vmcb;

#[unsafe(no_mangle)]
#[inline(never)]
pub unsafe fn vmrun(vmcb_phys_addr: u64) {
    unsafe {
        asm!("mov rax, {0}", in(reg) vmcb_phys_addr);

        asm!("vmload");
        asm!("vmrun");
        asm!("vmsave");

        info!("vmrun returned");
    }
}
