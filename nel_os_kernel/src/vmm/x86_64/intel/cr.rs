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
    match qual.access_type() {
        AccessType::MovTo => match qual.index() {
            0 | 4 => {
                passthrough_write(vcpu, qual)?;
                update_ia32e(vcpu)?;
            }
            8 => {
                let value = get_value(vcpu, qual)?;
                vcpu.guest_cr8 = validate_cr8(value)?;
            }
            _ => return Err("Unsupported guest CR write"),
        },
        AccessType::MovFrom => passthrough_read(vcpu, qual)?,
        _ => return Err("Unsupported guest CR access type"),
    }

    Ok(())
}

fn passthrough_read(vcpu: &mut IntelVCpu, qual: &QualCr) -> Result<(), &'static str> {
    let value = match qual.index() {
        3 => vmread(x86::vmx::vmcs::guest::CR3)?,
        8 => u64::from(vcpu.guest_cr8),
        _ => return Err("Unsupported guest CR read"),
    };

    set_value(vcpu, qual, value)?;

    Ok(())
}

fn validate_cr8(value: u64) -> Result<u8, &'static str> {
    if value & !0xf != 0 {
        return Err("Guest CR8 write sets reserved bits");
    }
    Ok(value as u8)
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
        _ => return Err("Unsupported guest CR write"),
    }

    Ok(())
}

pub fn update_ia32e(vcpu: &mut IntelVCpu) -> Result<(), &'static str> {
    let cr0 = vmread(x86::vmx::vmcs::guest::CR0)?;
    let cr4 = vmread(x86::vmx::vmcs::guest::CR4)?;
    let mut efer = vmread(x86::vmx::vmcs::guest::IA32_EFER_FULL)?;
    let ia32e_enabled = ia32e_active(cr0, cr4, efer);

    vcpu.ia32e_enabled = ia32e_enabled;

    let mut entry_ctrl = super::vmcs::controls::EntryControls::read()?;
    entry_ctrl.set_ia32e_mode_guest(ia32e_enabled);
    entry_ctrl.write()?;

    // LME is software-controlled through WRMSR. LMA is read-only state
    // derived from LME, CR0.PG, and CR4.PAE.
    if ia32e_enabled {
        efer |= 1 << 10;
    } else {
        efer &= !(1 << 10);
    }
    vmwrite(x86::vmx::vmcs::guest::IA32_EFER_FULL, efer)?;

    Ok(())
}

fn ia32e_active(cr0: u64, cr4: u64, efer: u64) -> bool {
    cr0 & (1 << 31) != 0 && cr4 & (1 << 5) != 0 && efer & (1 << 8) != 0
}

pub fn cr0_guest_host_mask() -> u64 {
    guest_host_mask(
        read_msr(x86::msr::IA32_VMX_CR0_FIXED0),
        read_msr(x86::msr::IA32_VMX_CR0_FIXED1),
        (1 << 0) | (1 << 31),
    )
}

pub fn cr4_guest_host_mask() -> u64 {
    guest_host_mask(
        read_msr(x86::msr::IA32_VMX_CR4_FIXED0),
        read_msr(x86::msr::IA32_VMX_CR4_FIXED1),
        1 << 5,
    )
}

fn guest_host_mask(fixed0: u64, fixed1: u64, mode_bits: u64) -> u64 {
    // Trap bits whose VMX fixed value must be synthesized, reserved bits that
    // cannot enter the VMCS, and the mode bits that update IA-32e state. Other
    // architectural bits (notably CR0.WP) can execute directly in the guest.
    fixed0 | !fixed1 | mode_bits
}

pub fn adjust_cr0(value: u64) -> u64 {
    let mut result = value;

    let cr0_fixed0 = read_msr(x86::msr::IA32_VMX_CR0_FIXED0);
    let cr0_fixed1 = read_msr(x86::msr::IA32_VMX_CR0_FIXED1);

    // With unrestricted-guest execution, PE and PG are explicitly exempt
    // from IA32_VMX_CR0_FIXED0. All other required bits (notably NE on older
    // Intel CPUs) still have to be set for VM entry.
    let unrestricted_exceptions = (1 << 0) | (1 << 31);
    result |= cr0_fixed0 & !unrestricted_exceptions;
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

#[cfg(test)]
mod tests {
    use super::{guest_host_mask, ia32e_active, validate_cr8};

    #[test]
    fn cr8_accepts_all_architectural_priority_values() {
        for value in 0..=15 {
            assert_eq!(validate_cr8(value), Ok(value as u8));
        }
    }

    #[test]
    fn cr8_rejects_reserved_bits() {
        assert!(validate_cr8(16).is_err());
        assert!(validate_cr8(u64::MAX).is_err());
    }

    #[test]
    fn long_mode_requires_paging_pae_and_lme() {
        assert!(ia32e_active(1 << 31, 1 << 5, 1 << 8));
        assert!(!ia32e_active(0, 1 << 5, 1 << 8));
        assert!(!ia32e_active(1 << 31, 0, 1 << 8));
        assert!(!ia32e_active(1 << 31, 1 << 5, 0));
    }

    #[test]
    fn guest_host_mask_traps_fixed_reserved_and_mode_bits_only() {
        let fixed0 = 1 << 13;
        let fixed1 = (1 << 5) | (1 << 13) | (1 << 16);
        let mask = guest_host_mask(fixed0, fixed1, 1 << 5);

        assert_ne!(mask & (1 << 5), 0);
        assert_ne!(mask & (1 << 13), 0);
        assert_eq!(mask & (1 << 16), 0);
        assert_ne!(mask & (1 << 20), 0);
    }
}
