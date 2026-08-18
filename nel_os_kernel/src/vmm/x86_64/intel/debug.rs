use x86::vmx::vmcs;

use crate::vmm::x86_64::intel::{vcpu::IntelVCpu, vmread, vmwrite};

const CR4_DEBUGGING_EXTENSIONS: u64 = 1 << 3;
const DR6_DEBUG_REGISTER_ACCESS: u64 = 1 << 13;
const DR6_RESET: u64 = 0xffff_0ff0;
const DR6_VOLATILE: u64 = 0x0001_e80f;
const DR7_GENERAL_DETECT: u64 = 1 << 13;
const DR7_FIXED_ONE: u64 = 0x400;
const DR7_VOLATILE: u64 = 0xffff_2bff;

pub const RESET_DEBUG_REGISTERS: [u64; 8] = [0, 0, 0, 0, 0, 0, DR6_RESET, DR7_FIXED_ONE];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugAccessOutcome {
    Completed,
    GeneralProtection,
    InvalidOpcode,
    DebugException,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DebugQualification {
    register: u8,
    read: bool,
    general_register: u8,
}

impl DebugQualification {
    fn decode(raw: u64) -> Self {
        Self {
            register: (raw & 0x7) as u8,
            read: raw & (1 << 4) != 0,
            general_register: ((raw >> 8) & 0xf) as u8,
        }
    }
}

pub fn handle_debug_register_access(
    vcpu: &mut IntelVCpu,
    raw_qualification: u64,
) -> Result<DebugAccessOutcome, &'static str> {
    let qualification = DebugQualification::decode(raw_qualification);

    // DR4/DR5 alias DR6/DR7 unless CR4.DE makes their encodings invalid.
    // #UD has priority over the privilege check.
    let debugging_extensions = vmread(vmcs::guest::CR4)? & CR4_DEBUGGING_EXTENSIONS != 0;
    let Some(register) = architectural_register(qualification.register, debugging_extensions)
    else {
        return Ok(DebugAccessOutcome::InvalidOpcode);
    };

    // MOV DR is privileged. VM exits occur before the instruction can affect
    // host debug state, so reproduce the architectural CPL check here.
    if vmread(vmcs::guest::CS_SELECTOR)? & 3 != 0 {
        return Ok(DebugAccessOutcome::GeneralProtection);
    }

    if vcpu.guest_debug_registers[7] & DR7_GENERAL_DETECT != 0 {
        vcpu.guest_debug_registers[7] &= !DR7_GENERAL_DETECT;
        vcpu.guest_debug_registers[6] |= DR6_DEBUG_REGISTER_ACCESS;
        return Ok(DebugAccessOutcome::DebugException);
    }

    if qualification.read {
        set_general_register(
            vcpu,
            qualification.general_register,
            vcpu.guest_debug_registers[register],
        )?;
    } else {
        let value = get_general_register(vcpu, qualification.general_register)?;
        if write_debug_register(&mut vcpu.guest_debug_registers, register, value).is_err() {
            return Ok(DebugAccessOutcome::GeneralProtection);
        }
    }

    Ok(DebugAccessOutcome::Completed)
}

fn write_debug_register(registers: &mut [u64; 8], register: usize, value: u64) -> Result<(), ()> {
    match register {
        0..=3 => registers[register] = value,
        6 => {
            if value >> 32 != 0 {
                return Err(());
            }
            registers[6] = (value & DR6_VOLATILE) | (DR6_RESET & !DR6_VOLATILE);
        }
        7 => {
            if value >> 32 != 0 {
                return Err(());
            }
            registers[7] = (value & DR7_VOLATILE) | DR7_FIXED_ONE;
        }
        _ => return Err(()),
    }
    Ok(())
}

fn architectural_register(register: u8, debugging_extensions: bool) -> Option<usize> {
    match register {
        0..=3 | 6 | 7 => Some(register as usize),
        4 if !debugging_extensions => Some(6),
        5 if !debugging_extensions => Some(7),
        _ => None,
    }
}

fn set_general_register(
    vcpu: &mut IntelVCpu,
    register: u8,
    value: u64,
) -> Result<(), &'static str> {
    let guest = &mut vcpu.guest_registers;
    match register {
        0 => guest.rax = value,
        1 => guest.rcx = value,
        2 => guest.rdx = value,
        3 => guest.rbx = value,
        4 => vmwrite(vmcs::guest::RSP, value)?,
        5 => guest.rbp = value,
        6 => guest.rsi = value,
        7 => guest.rdi = value,
        8 => guest.r8 = value,
        9 => guest.r9 = value,
        10 => guest.r10 = value,
        11 => guest.r11 = value,
        12 => guest.r12 = value,
        13 => guest.r13 = value,
        14 => guest.r14 = value,
        15 => guest.r15 = value,
        _ => return Err("Invalid general register in MOV-DR qualification"),
    }
    Ok(())
}

fn get_general_register(vcpu: &IntelVCpu, register: u8) -> Result<u64, &'static str> {
    let guest = &vcpu.guest_registers;
    Ok(match register {
        0 => guest.rax,
        1 => guest.rcx,
        2 => guest.rdx,
        3 => guest.rbx,
        4 => vmread(vmcs::guest::RSP)?,
        5 => guest.rbp,
        6 => guest.rsi,
        7 => guest.rdi,
        8 => guest.r8,
        9 => guest.r9,
        10 => guest.r10,
        11 => guest.r11,
        12 => guest.r12,
        13 => guest.r13,
        14 => guest.r14,
        15 => guest.r15,
        _ => return Err("Invalid general register in MOV-DR qualification"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mov_dr_exit_qualification() {
        assert_eq!(
            DebugQualification::decode(7 | (1 << 4) | (13 << 8)),
            DebugQualification {
                register: 7,
                read: true,
                general_register: 13,
            }
        );
    }

    #[test]
    fn dr4_and_dr5_alias_only_without_debugging_extensions() {
        assert_eq!(architectural_register(4, false), Some(6));
        assert_eq!(architectural_register(5, false), Some(7));
        assert_eq!(architectural_register(4, true), None);
        assert_eq!(architectural_register(5, true), None);
    }

    #[test]
    fn dr6_and_dr7_writes_enforce_reserved_bits() {
        let mut registers = [0; 8];
        assert!(write_debug_register(&mut registers, 6, u64::MAX).is_err());
        assert!(write_debug_register(&mut registers, 7, u64::MAX).is_err());

        write_debug_register(&mut registers, 6, 0).unwrap();
        write_debug_register(&mut registers, 7, 0).unwrap();
        assert_eq!(registers[6], DR6_RESET & !DR6_VOLATILE);
        assert_eq!(registers[7], DR7_FIXED_ONE);
    }
}
