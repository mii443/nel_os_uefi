use core::arch::asm;

pub mod vcpu;
pub mod vmcb;

#[unsafe(no_mangle)]
#[inline(never)]
pub unsafe fn vmrun(vmcb_phys_addr: u64) {
    unsafe {
        asm!(
            "push rbx",
            "push rcx",
            "push rdx",
            "push rsi",
            "push rdi",
            "push r8",
            "push r9",
            "push r10",
            "push r11",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "push rbp",
            "mov rax, {vmcb}",
            "vmrun",
            "pop rbp",
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop r11",
            "pop r10",
            "pop r9",
            "pop r8",
            "pop rdi",
            "pop rsi",
            "pop rdx",
            "pop rcx",
            "pop rbx",
            vmcb = in(reg) vmcb_phys_addr,
            lateout("rax") _,
            options(preserves_flags),
        );
    }
}
