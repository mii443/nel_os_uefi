pub mod asm;
mod controls;
mod cpuid;
mod ept;
mod msr;
mod register;
pub mod vcpu;
mod vmcs;
mod vmexit;
mod vmxon;

use core::arch::asm;

use x86_64::registers::rflags::{self, RFlags};

pub fn vmx_capture_status() -> Result<(), &'static str> {
    let flags = rflags::read();
    if flags.contains(RFlags::ZERO_FLAG) {
        Err("VM fail valid")
    } else if flags.contains(RFlags::CARRY_FLAG) {
        Err("VM fail invalid")
    } else {
        Ok(())
    }
}

pub fn vmread(field: u32) -> Result<u64, &'static str> {
    let field: u64 = field.into();
    let value: u64;
    unsafe {
        asm!(
            "vmread {0}, {1}",
            in(reg) field,
            out(reg) value,
            options(att_syntax)
        )
    };
    vmx_capture_status()?;
    Ok(value)
}

pub fn vmwrite(field: u32, value: u64) -> Result<(), &'static str> {
    let field: u64 = field.into();
    unsafe {
        asm!(
            "vmwrite {1}, {0}",
            in(reg) field,
            in(reg) value,
            options(att_syntax)
        )
    };
    vmx_capture_status()
}
