use crate::vmm::x86_64::{
    common,
    intel::{vmcs, vmwrite},
};

pub fn setup_exec_controls() -> Result<(), &'static str> {
    let basic_msr = common::read_msr(0x480);
    let mut raw_pin_exec_ctrl = u32::from(vmcs::controls::PinBasedVmExecutionControls::read()?);

    let reserved_bits = if basic_msr & (1 << 55) != 0 {
        common::read_msr(0x48d)
    } else {
        common::read_msr(0x481)
    };
    raw_pin_exec_ctrl |= (reserved_bits & 0xFFFFFFFF) as u32;
    raw_pin_exec_ctrl &= (reserved_bits >> 32) as u32;

    let mut pin_exec_ctrl = vmcs::controls::PinBasedVmExecutionControls::from(raw_pin_exec_ctrl);
    pin_exec_ctrl.set_external_interrupt_exiting(false);

    pin_exec_ctrl.write();

    let mut raw_primary_exec_ctrl =
        u32::from(vmcs::controls::PrimaryProcessorBasedVmExecutionControls::read()?);

    let reserved_bits = if basic_msr & (1 << 55) != 0 {
        common::read_msr(0x48e)
    } else {
        common::read_msr(0x482)
    };
    raw_primary_exec_ctrl |= (reserved_bits & 0xFFFFFFFF) as u32;
    raw_primary_exec_ctrl &= (reserved_bits >> 32) as u32;

    let mut primary_exec_ctrl =
        vmcs::controls::PrimaryProcessorBasedVmExecutionControls::from(raw_primary_exec_ctrl);
    primary_exec_ctrl.set_hlt(true);
    primary_exec_ctrl.set_activate_secondary_controls(true);
    primary_exec_ctrl.set_use_tpr_shadow(false);
    primary_exec_ctrl.set_use_msr_bitmap(false);
    primary_exec_ctrl.set_unconditional_io(false);
    primary_exec_ctrl.set_use_io_bitmap(false); // TODO: true

    primary_exec_ctrl.write();

    let mut raw_secondary_exec_ctrl =
        u32::from(vmcs::controls::SecondaryProcessorBasedVmExecutionControls::read()?);

    let reserved_bits = if basic_msr & (1 << 55) != 0 {
        common::read_msr(0x48b)
    } else {
        0
    };
    raw_secondary_exec_ctrl |= (reserved_bits & 0xFFFFFFFF) as u32;
    raw_secondary_exec_ctrl &= (reserved_bits >> 32) as u32;

    let mut secondary_exec_ctrl =
        vmcs::controls::SecondaryProcessorBasedVmExecutionControls::from(raw_secondary_exec_ctrl);
    secondary_exec_ctrl.set_ept(false); // TODO: true
    secondary_exec_ctrl.set_unrestricted_guest(false); //TODO: true
    secondary_exec_ctrl.set_virtualize_apic_accesses(false); // TODO: true

    secondary_exec_ctrl.write();

    vmwrite(0x6000, u64::MAX)?;
    vmwrite(0x6002, u64::MAX)?;

    Ok(())
}

pub fn setup_entry_controls() -> Result<(), &'static str> {
    let baisc_msr = common::read_msr(0x480);

    let mut raw_entry_ctrl = u32::from(vmcs::controls::EntryControls::read()?);
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

    entry_ctrl.write();

    Ok(())
}

pub fn setup_exit_controls() -> Result<(), &'static str> {
    let basic_msr = common::read_msr(0x480);

    let mut raw_exit_ctrl = u32::from(vmcs::controls::PrimaryExitControls::read()?);
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

    exit_ctrl.write();

    vmwrite(0x4004, 1u64 << 6)?; // EXCEPTION_BITMAP

    Ok(())
}
