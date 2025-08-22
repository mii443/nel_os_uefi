use x86::vmx::vmcs;

use crate::vmm::x86_64::{
    common::read_msr,
    intel::{
        qual::{AccessType, QualCr, Register},
        vcpu::IntelVCpu,
        vmread, vmwrite,
    },
};

pub fn handle_cr_access(vcpu: &mut IntelVCpu, qual: &QualCr) -> Result<(), &'static str> {
    match AccessType::try_from(qual.access_type()).unwrap() {
        AccessType::MovTo => match qual.index() {
            0 | 4 => {
                passthrough_write(vcpu, qual);
                update_ia32e(vcpu);
            }
            _ => panic!("Unsupported CR index: {}", qual.index()),
        },
        AccessType::MovFrom => passthrough_read(vcpu, qual)?,
        _ => {
            panic!("Unsupported CR access type: {:?}", qual.access_type());
        }
    }

    Ok(())
}

fn passthrough_read(vcpu: &mut IntelVCpu, qual: &QualCr) -> Result<(), &'static str> {
    let value = match qual.index() {
        3 => vmread(x86::vmx::vmcs::guest::CR3)?,
        _ => panic!("Unsupported CR index: {}", qual.index()),
    };

    set_value(vcpu, qual, value);

    Ok(())
}

fn passthrough_write(vcpu: &mut IntelVCpu, qual: &QualCr) -> Result<(), &'static str> {
    let value = get_value(vcpu, qual)?;
    match qual.index() {
        0 => {
            vmwrite(vmcs::guest::CR0, adjust_cr0(value))?;
            vmwrite(vmcs::control::CR0_READ_SHADOW, value)?;
        }
        4 => {
            vmwrite(vmcs::guest::CR4, adjust_cr4(value))?;
            vmwrite(vmcs::control::CR4_READ_SHADOW, value)?;
        }
        _ => {
            panic!("Unsupported CR index: {}", qual.index());
        }
    }

    Ok(())
}

pub fn update_ia32e(vcpu: &mut IntelVCpu) -> Result<(), &'static str> {
    let cr0 = vmread(x86::vmx::vmcs::guest::CR0)?;
    let cr4 = vmread(x86::vmx::vmcs::guest::CR4)?;
    let ia32e_enabled = (cr0 & 1 << 31) != 0 && (cr4 & 1 << 5) != 0;

    vcpu.ia32e_enabled = ia32e_enabled;

    let mut entry_ctrl = super::vmcs::controls::EntryControls::read()?;
    entry_ctrl.set_ia32e_mode_guest(ia32e_enabled);
    entry_ctrl.write();

    let mut efer = vmread(x86::vmx::vmcs::guest::IA32_EFER_FULL)?;

    let lma = (vcpu.ia32e_enabled as u64) << 10;
    if lma != 0 {
        efer |= lma;
    } else {
        efer &= !lma;
    }

    let lme = if cr0 & (1 << 31) != 0 {
        efer & (1 << 10)
    } else {
        efer & !(1 << 8)
    };
    if lme != 0 {
        efer |= lme;
    } else {
        efer &= lme;
    }

    vmwrite(x86::vmx::vmcs::guest::IA32_EFER_FULL, efer)?;

    Ok(())
}

pub fn adjust_cr0(value: u64) -> u64 {
    let mut result = value;

    let cr0_fixed0 = read_msr(x86::msr::IA32_VMX_CR0_FIXED0);
    let cr0_fixed1 = read_msr(x86::msr::IA32_VMX_CR0_FIXED1);

    result |= cr0_fixed0;
    result &= cr0_fixed1;

    result
}

pub fn adjust_cr4(value: u64) -> u64 {
    let mut result = value;

    let cr4_fixed0 = read_msr(x86::msr::IA32_VMX_CR4_FIXED0);
    let cr4_fixed1 = read_msr(x86::msr::IA32_VMX_CR4_FIXED1);

    result |= cr4_fixed0;
    result &= cr4_fixed1;

    result
}

fn set_value(vcpu: &mut IntelVCpu, qual: &QualCr, value: u64) -> Result<(), &'static str> {
    let guest_regs = &mut vcpu.guest_registers;

    match qual.register() {
        Register::Rax => guest_regs.rax = value,
        Register::Rcx => guest_regs.rcx = value,
        Register::Rdx => guest_regs.rdx = value,
        Register::Rbx => guest_regs.rbx = value,
        Register::Rbp => guest_regs.rbp = value,
        Register::Rsi => guest_regs.rsi = value,
        Register::Rdi => guest_regs.rdi = value,
        Register::R8 => guest_regs.r8 = value,
        Register::R9 => guest_regs.r9 = value,
        Register::R10 => guest_regs.r10 = value,
        Register::R11 => guest_regs.r11 = value,
        Register::R12 => guest_regs.r12 = value,
        Register::R13 => guest_regs.r13 = value,
        Register::R14 => guest_regs.r14 = value,
        Register::R15 => guest_regs.r15 = value,
        Register::Rsp => vmwrite(x86::vmx::vmcs::guest::RSP, value)?,
    }

    Ok(())
}

fn get_value(vcpu: &mut IntelVCpu, qual: &QualCr) -> Result<u64, &'static str> {
    let guest_regs = &mut vcpu.guest_registers;

    Ok(match qual.register() {
        Register::Rax => guest_regs.rax,
        Register::Rcx => guest_regs.rcx,
        Register::Rdx => guest_regs.rdx,
        Register::Rbx => guest_regs.rbx,
        Register::Rbp => guest_regs.rbp,
        Register::Rsi => guest_regs.rsi,
        Register::Rdi => guest_regs.rdi,
        Register::R8 => guest_regs.r8,
        Register::R9 => guest_regs.r9,
        Register::R10 => guest_regs.r10,
        Register::R11 => guest_regs.r11,
        Register::R12 => guest_regs.r12,
        Register::R13 => guest_regs.r13,
        Register::R14 => guest_regs.r14,
        Register::R15 => guest_regs.r15,
        Register::Rsp => vmread(x86::vmx::vmcs::guest::RSP)?,
    })
}
