use crate::vmm::x86_64::intel::{register::GuestRegisters, vcpu::IntelVCpu};
use core::{arch::global_asm, mem::offset_of};

#[allow(improper_ctypes)]
unsafe extern "C" {
    pub unsafe fn asm_vm_entry(vcpu: *mut IntelVCpu) -> u16;
    pub unsafe fn asm_vmexit_handler() -> !;
}

global_asm!(
".global asm_vm_entry",
".type asm_vm_entry, @function",
"asm_vm_entry:", // rdi = *VCpu
"push rbp",
"push r15",
"push r14",
"push r13",
"push r12",
"push rbx",
"cmp qword ptr [rdi + {host_xsave_mask_offset}], 0",
"jne 3f",
"fxsave64 [rdi + {host_fx_state_offset}]",
"3:",
/*
   stack:
   +-----+
   | RBX |
   +-----+
   | R12 |
   +-----+
   | R13 |
   +-----+
   | R14 |
   +-----+
   | R15 |
   +-----+
   | RBP |
   +-----+
   | RIP |
   +-----+

   regs:
   RDI = *VCpu
   */

"lea rbx, [rdi + {guest_regs_offset}]", // rbx = *guest_regs
"push rbx", // push *guest_regs
"push rdi", // push *VCpu
"lea rdi, [rsp + 8]", // rdi = rsp + 8 = *guest_regs
// SysV requires RSP to be 16-byte aligned immediately before CALL.
"sub rsp, 8",
"call intel_set_host_stack",
"add rsp, 8",
"pop rdi", // rdi = *VCpu
// VMX does not switch CR2.  Save the host value and restore this VCPU's
// page-fault address before loading guest GPRs.
"mov rax, cr2",
"mov [rdi + {host_cr2_offset}], rax",
"mov rax, [rdi + {guest_cr2_offset}]",
"mov cr2, rax",
"test byte ptr [rdi + {launch_done_offset}], 1", // flag = launch_done ? 1 : 0
"fxrstor64 [rdi + {guest_fx_state_offset}]",
/*
   stack:
   +-------------+
   | *guest_regs |
   +-------------+
   |     RBX     |
   +-------------+
   |     R12     |
   +-------------+
   |     R13     |
   +-------------+
   |     R14     |
   +-------------+
   |     R15     |
   +-------------+
   |     RBP     |
   +-------------+
   |     RIP     |
   +-------------+
   regs:
   RDI = *VCpu
   RBX = *guest_regs
   */
"mov rax, rbx", // rax = *guest_regs
"mov rcx, [rax+{reg_offset_rcx}]", // rcx = guest_regs.rcx
"mov rdx, [rax+{reg_offset_rdx}]", // rdx = guest_regs.rdx
"mov rbx, [rax+{reg_offset_rbx}]", // rbx = guest_regs.rbx
"mov rsi, [rax+{reg_offset_rsi}]    ", // rsi = guest_regs.rsi

"mov rdi, [rax+{reg_offset_rdi}]", // rdi = guest_regs.rdi
"mov rbp, [rax+{reg_offset_rbp}]", // rbp = guest_regs.rbp
"mov r8, [rax+{reg_offset_r8}]", // r8 = guest_regs.r8
"mov r9, [rax+{reg_offset_r9}]", // r9 = guest_regs.r9

"mov r10, [rax+{reg_offset_r10}]", // r10 = guest_regs.r10
"mov r11, [rax+{reg_offset_r11}]", // r11 = guest_regs.r11
"mov r12, [rax+{reg_offset_r12}]", // r12 = guest_regs.r12
"mov r13, [rax+{reg_offset_r13}]", // r13 = guest_regs.r13
"mov r14, [rax+{reg_offset_r14}]", // r14 = guest_regs.r14
"mov r15, [rax+{reg_offset_r15}]", // r15 = guest_regs.r15
"mov rax, [rax+{reg_offset_rax}]", // rax = guest_regs.rax
/*
   stack:
   +-------------+
   | *guest_regs |
   +-------------+
   |     RBX     |
   +-------------+
   |     R12     |
   +-------------+
   |     R13     |
   +-------------+
   |     R14     |
   +-------------+
   |     R15     |
   +-------------+
   |     RBP     |
   +-------------+
   |     RIP     |
   +-------------+
   */
"jz 2f",
"vmresume",
"2:",
"vmlaunch",
"mov rax, [rsp]", // rax = *guest_regs
"sub rax, {guest_regs_offset}", // rax = *VCpu
// VM-entry failed after guest CR2 was installed, so restore host CR2 before
// returning to Rust even though no normal VMEXIT occurred.
"mov rcx, [rax + {host_cr2_offset}]",
"mov cr2, rcx",
"mov rdi, rax",
"call 4f",
"mov ax, 1",
"add rsp, 0x8",
"pop rbx",
"pop r12",
"pop r13",
"pop r14",
"pop r15",
"pop rbp",
"ret",

".size asm_vm_entry, . - asm_vm_entry",

".global asm_vmexit_handler",
".type asm_vmexit_handler, @function",
"asm_vmexit_handler:",
"cli",
/*
   stack:
   +-------------+
   | *guest_regs |
   +-------------+
   |     RBX     |
   +-------------+
   |     R12     |
   +-------------+
   |     R13     |
   +-------------+

   |     R14     |
   +-------------+
   |     R15     |
   +-------------+
   |     RBP     |
   +-------------+
   |     RIP     |
   +-------------+

   regs:
   RAX = guest CPU's rax
   */
"push rax",
"push rcx",
"mov rax, qword ptr [rsp + 0x10]", // rax = *guest_regs
"sub rax, {guest_regs_offset}", // rax = *VCpu
// Capture the guest fault address before any host work can change CR2, then
// restore the value that was active before this VCPU entered VMX non-root.
"mov rcx, cr2",
"mov [rax + {guest_cr2_offset}], rcx",
"mov rcx, [rax + {host_cr2_offset}]",
"mov cr2, rcx",
"pop rcx",
"fxsave64 [rax + {guest_fx_state_offset}]",
"add rax, {guest_regs_offset}", // rax = *guest_regs
/*
   stack:
   +-------------+

   |  guest RAX  |
   +-------------+
   | *guest_regs |
   +-------------+
   |     RBX     |
   +-------------+
   |     R12     |
   +-------------+
   |     R13     |
   +-------------+
   |     R14     |
   +-------------+
   |     R15     |

   +-------------+
   |     RBP     |
   +-------------+
   |     RIP     |
   +-------------+
   */


"pop [rax + {reg_offset_rax}]", // guest_regs.rax = guest CPU's rax
"add rsp, 0x8", // discard *guest_regs
/*
   stack:
   +-------------+
   |     RBX     |
   +-------------+
   |     R12     |
   +-------------+
   |     R13     |
   +-------------+
   |     R14     |
   +-------------+
   |     R15     |
   +-------------+
   |     RBP     |
   +-------------+
   |     RIP     |
   +-------------+
   */

// save rcx, rdx, rbx, rsi, rdi, rbp, r8~15
"mov [rax + {reg_offset_rcx}], rcx",
"mov [rax + {reg_offset_rdx}], rdx",
"mov [rax + {reg_offset_rbx}], rbx",
"mov [rax + {reg_offset_rsi}], rsi",

"mov [rax + {reg_offset_rdi}], rdi",
"mov [rax + {reg_offset_rbp}], rbp",
"mov [rax + {reg_offset_r8}], r8",
"mov [rax + {reg_offset_r9}], r9",
"mov [rax + {reg_offset_r10}], r10",
"mov [rax + {reg_offset_r11}], r11",
"mov [rax + {reg_offset_r12}], r12",
"mov [rax + {reg_offset_r13}], r13",
"mov [rax + {reg_offset_r14}], r14",
"mov [rax + {reg_offset_r15}], r15",

"mov rdi, rax",
"sub rdi, {guest_regs_offset}", // rdi = *VCpu
"call 4f",

"pop rbx",
"pop r12",
"pop r13",
"pop r14",
"pop r15",
"pop rbp",
/*

   stack:
   +-------------+
   |     RIP     |
   +-------------+
   */
"mov rax, 0x0",
"ret",

// Restore all host processor state. rdi = *VCpu. Guest GPRs have already
// been saved, so the XSETBV/XRSTOR operands may freely use caller-saved regs.
"4:",
"mov rax, [rdi + {host_xsave_mask_offset}]",
"test rax, rax",
"jz 5f",
"mov rdx, rax",
"shr rdx, 32",
"xor ecx, ecx",
"xsetbv",
"mov rax, [rdi + {host_xsave_mask_offset}]",
"mov rdx, rax",
"shr rdx, 32",
"mov rsi, [rdi + {host_xsave_addr_offset}]",
"xrstor64 [rsi]",
"ret",
"5:",
"fxrstor64 [rdi + {host_fx_state_offset}]",
"ret",


".size asm_vmexit_handler, . - asm_vmexit_handler",

guest_regs_offset = const offset_of!(IntelVCpu, guest_registers),
host_cr2_offset = const offset_of!(IntelVCpu, host_cr2),
guest_cr2_offset = const offset_of!(IntelVCpu, guest_cr2),
host_fx_state_offset = const offset_of!(IntelVCpu, host_fx_state),
guest_fx_state_offset = const offset_of!(IntelVCpu, guest_fx_state),
host_xsave_addr_offset = const offset_of!(IntelVCpu, host_xsave_addr),
host_xsave_mask_offset = const offset_of!(IntelVCpu, host_xsave_mask),
launch_done_offset = const offset_of!(IntelVCpu, launch_done),
reg_offset_rax = const offset_of!(GuestRegisters, rax),
reg_offset_rcx = const offset_of!(GuestRegisters, rcx),
reg_offset_rdx = const offset_of!(GuestRegisters, rdx),
reg_offset_rbx = const offset_of!(GuestRegisters, rbx),
reg_offset_rsi = const offset_of!(GuestRegisters, rsi),
reg_offset_rdi = const offset_of!(GuestRegisters, rdi),
reg_offset_rbp = const offset_of!(GuestRegisters, rbp),
reg_offset_r8 = const offset_of!(GuestRegisters, r8),

reg_offset_r9 = const offset_of!(GuestRegisters, r9),
reg_offset_r10 = const offset_of!(GuestRegisters, r10),
reg_offset_r11 = const offset_of!(GuestRegisters, r11),
reg_offset_r12 = const offset_of!(GuestRegisters, r12),
reg_offset_r13 = const offset_of!(GuestRegisters, r13),
reg_offset_r14 = const offset_of!(GuestRegisters, r14),
reg_offset_r15 = const offset_of!(GuestRegisters, r15),
);
