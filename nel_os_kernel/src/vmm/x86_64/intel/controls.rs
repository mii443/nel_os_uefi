use crate::vmm::x86_64::{
    common,
    intel::{vmcs, vmwrite},
};

const VMX_PREEMPTION_TIMER: u32 = 1 << 6;
const CR8_LOAD_EXITING: u32 = 1 << 19;
const CR8_STORE_EXITING: u32 = 1 << 20;
const MOV_DR_EXITING: u32 = 1 << 23;
const ENABLE_RDTSCP: u32 = 1 << 3;

fn apply_vmx_fixed_bits(value: u32, capability_msr: u64) -> u32 {
    let must_be_one = capability_msr as u32;
    let may_be_one = (capability_msr >> 32) as u32;
    (value | must_be_one) & may_be_one
}

fn require_allowed_one(
    capability_msr: u64,
    required: u32,
    error: &'static str,
) -> Result<(), &'static str> {
    let may_be_one = (capability_msr >> 32) as u32;
    if may_be_one & required != required {
        return Err(error);
    }
    Ok(())
}

pub fn preemption_timer_ticks(tsc_khz: u64, slice_millis: u64, timer_shift: u8) -> u32 {
    let tsc_cycles = tsc_khz.saturating_mul(slice_millis);
    let timer_quantum = 1u64 << timer_shift.min(63);
    let ticks = (tsc_cycles / timer_quantum)
        .saturating_add(u64::from(tsc_cycles % timer_quantum != 0))
        .max(1);
    ticks.min(u32::MAX as u64) as u32
}

pub fn setup_exec_controls() -> Result<u8, &'static str> {
    let basic_msr = common::read_msr(0x480);
    // VMCS fields are architecturally undefined after VMCLEAR. Build every
    // control value from the capability MSR rather than preserving whatever
    // VMREAD happens to return for a newly created VMCS.
    let mut raw_pin_exec_ctrl = 0;

    let pin_capabilities = if basic_msr & (1 << 55) != 0 {
        common::read_msr(0x48d)
    } else {
        common::read_msr(0x481)
    };
    require_allowed_one(
        pin_capabilities,
        VMX_PREEMPTION_TIMER,
        "VMX preemption timer is required but unsupported",
    )?;
    raw_pin_exec_ctrl = apply_vmx_fixed_bits(raw_pin_exec_ctrl, pin_capabilities);

    let mut pin_exec_ctrl = vmcs::controls::PinBasedVmExecutionControls::from(raw_pin_exec_ctrl);
    pin_exec_ctrl.set_external_interrupt_exiting(true);
    pin_exec_ctrl.set_activate_vmx_preemption_timer(true);

    pin_exec_ctrl.write()?;

    let mut raw_primary_exec_ctrl = 0;

    let primary_capabilities = if basic_msr & (1 << 55) != 0 {
        common::read_msr(0x48e)
    } else {
        common::read_msr(0x482)
    };
    require_allowed_one(
        primary_capabilities,
        CR8_LOAD_EXITING | CR8_STORE_EXITING,
        "CR8 load/store exiting is required but unsupported",
    )?;
    require_allowed_one(
        primary_capabilities,
        MOV_DR_EXITING,
        "MOV-DR exiting is required but unsupported",
    )?;
    raw_primary_exec_ctrl = apply_vmx_fixed_bits(raw_primary_exec_ctrl, primary_capabilities);

    let mut primary_exec_ctrl =
        vmcs::controls::PrimaryProcessorBasedVmExecutionControls::from(raw_primary_exec_ctrl);
    primary_exec_ctrl.set_hlt(true);
    primary_exec_ctrl.set_activate_secondary_controls(true);
    primary_exec_ctrl.set_use_msr_bitmap(false);
    primary_exec_ctrl.set_unconditional_io(false);
    primary_exec_ctrl.set_use_io_bitmap(true);
    primary_exec_ctrl.set_cr8load(true);
    primary_exec_ctrl.set_cr8store(true);
    primary_exec_ctrl.set_mov_dr(true);

    primary_exec_ctrl.write()?;

    let mut raw_secondary_exec_ctrl = 0;

    let secondary_capabilities = if basic_msr & (1 << 55) != 0 {
        common::read_msr(x86::msr::IA32_VMX_PROCBASED_CTLS2)
    } else {
        0
    };
    raw_secondary_exec_ctrl = apply_vmx_fixed_bits(raw_secondary_exec_ctrl, secondary_capabilities);

    let mut secondary_exec_ctrl =
        vmcs::controls::SecondaryProcessorBasedVmExecutionControls::from(raw_secondary_exec_ctrl);
    secondary_exec_ctrl.set_ept(true);
    secondary_exec_ctrl.set_rdtscp(((secondary_capabilities >> 32) as u32) & ENABLE_RDTSCP != 0);
    secondary_exec_ctrl.set_unrestricted_guest(true);
    //secondary_exec_ctrl.set_virtualize_apic_accesses(false); // TODO: true

    secondary_exec_ctrl.write()?;

    vmwrite(
        x86::vmx::vmcs::control::CR0_GUEST_HOST_MASK,
        super::cr::cr0_guest_host_mask(),
    )?;
    vmwrite(
        x86::vmx::vmcs::control::CR4_GUEST_HOST_MASK,
        super::cr::cr4_guest_host_mask(),
    )?;

    Ok((common::read_msr(x86::msr::IA32_VMX_MISC) & 0x1f) as u8)
}

pub fn setup_entry_controls() -> Result<(), &'static str> {
    let baisc_msr = common::read_msr(0x480);

    let mut raw_entry_ctrl = 0;
    let reserved_bits = if baisc_msr & (1 << 55) != 0 {
        common::read_msr(0x490)
    } else {
        common::read_msr(0x484)
    };
    raw_entry_ctrl |= (reserved_bits & 0xFFFFFFFF) as u32;
    raw_entry_ctrl &= (reserved_bits >> 32) as u32;

    let mut entry_ctrl = vmcs::controls::EntryControls::from(raw_entry_ctrl);
    entry_ctrl.set_ia32e_mode_guest(false);
    entry_ctrl.set_load_ia32_efer(true);
    entry_ctrl.set_load_ia32_pat(true);

    entry_ctrl.write()?;

    Ok(())
}

pub fn setup_exit_controls() -> Result<(), &'static str> {
    let basic_msr = common::read_msr(0x480);

    let mut raw_exit_ctrl = 0;
    let reserved_bits = if basic_msr & (1 << 55) != 0 {
        common::read_msr(0x48f)
    } else {
        common::read_msr(0x483)
    };
    raw_exit_ctrl |= (reserved_bits & 0xFFFFFFFF) as u32;
    raw_exit_ctrl &= (reserved_bits >> 32) as u32;

    let mut exit_ctrl = vmcs::controls::PrimaryExitControls::from(raw_exit_ctrl);
    exit_ctrl.set_host_addr_space_size(true);
    exit_ctrl.set_save_ia32_efer(true);
    exit_ctrl.set_save_ia32_pat(true);
    exit_ctrl.set_load_ia32_efer(true);
    exit_ctrl.set_load_ia32_pat(true);

    exit_ctrl.write()?;

    vmwrite(
        x86::vmx::vmcs::control::EXCEPTION_BITMAP,
        1u64 << x86::irq::INVALID_OPCODE_VECTOR,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_control_bits_are_applied_without_setting_disallowed_bits() {
        let capabilities = (0b1011u64 << 32) | 0b0010;
        assert_eq!(apply_vmx_fixed_bits(0b1100, capabilities), 0b1010);
    }

    #[test]
    fn required_control_bits_must_all_be_allowed_one() {
        let capabilities = (CR8_LOAD_EXITING as u64) << 32;
        assert!(require_allowed_one(capabilities, CR8_LOAD_EXITING, "error").is_ok());
        assert!(
            require_allowed_one(capabilities, CR8_LOAD_EXITING | CR8_STORE_EXITING, "error")
                .is_err()
        );
    }

    #[test]
    fn timer_ticks_use_vmx_misc_shift_and_never_reach_zero() {
        assert_eq!(preemption_timer_ticks(1_000_000, 4, 0), 4_000_000);
        assert_eq!(preemption_timer_ticks(1_000_000, 4, 10), 3_907);
        assert_eq!(preemption_timer_ticks(1, 1, 31), 1);
        assert_eq!(preemption_timer_ticks(0, 0, 0), 1);
    }

    #[test]
    fn timer_ticks_saturate_to_vmcs_field_width() {
        assert_eq!(preemption_timer_ticks(u64::MAX, u64::MAX, 0), u32::MAX);
    }
}
