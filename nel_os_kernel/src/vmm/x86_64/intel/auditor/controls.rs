use x86::{msr, vmx::vmcs};

use crate::vmm::x86_64::{
    common::read_msr,
    intel::{self, vmread},
};

pub fn check_vmcs_control_fields() -> Result<(), &'static str> {
    let msr_ia32_vmx_basic = read_msr(msr::IA32_VMX_BASIC);
    let vmx_true_ctrl = msr_ia32_vmx_basic & (1 << 55) != 0;

    check_pin_based_exec_ctrl(vmx_true_ctrl)?;
    check_primary_proc_based_exec_ctrl(vmx_true_ctrl)?;

    if intel::vmcs::controls::PrimaryProcessorBasedVmExecutionControls::read()?
        .activate_secondary_controls()
    {
        check_secondary_proc_based_exec_ctrl(vmx_true_ctrl)?;
    }

    Ok(())
}

fn check_pin_based_exec_ctrl(vmx_true_ctrl: bool) -> Result<(), &'static str> {
    let msr_vmx_pin_based_exec_ctrl = if vmx_true_ctrl {
        read_msr(msr::IA32_VMX_TRUE_PINBASED_CTLS)
    } else {
        read_msr(msr::IA32_VMX_PINBASED_CTLS)
    };

    let vmcs_pin_based_exec_ctrl = vmread(vmcs::control::PINBASED_EXEC_CONTROLS)?;
    let msr_vmx_pin_based_exec_ctrl_low = (msr_vmx_pin_based_exec_ctrl & 0xFFFFFFFF) as u32;
    let msr_vmx_pin_based_exec_ctrl_high = (msr_vmx_pin_based_exec_ctrl >> 32) as u32;

    if !(vmcs_pin_based_exec_ctrl & 0xFFFFFFFF) as u32 & msr_vmx_pin_based_exec_ctrl_low != 0 {
        return Err(
            "VMCS Pin-based execution controls field: IA32_VMX_PINBASED_CTRLS low bits not set",
        );
    }
    if (vmcs_pin_based_exec_ctrl >> 32) as u32 & !msr_vmx_pin_based_exec_ctrl_high != 0 {
        return Err(
            "VMCS Pin-based execution controls field: IA32_VMX_PINBASED_CTRLS high bits not zero",
        );
    }

    Ok(())
}

fn check_primary_proc_based_exec_ctrl(vmx_true_ctrl: bool) -> Result<(), &'static str> {
    let msr_vmx_primary_proc_based_exec_ctrl = if vmx_true_ctrl {
        read_msr(msr::IA32_VMX_TRUE_PROCBASED_CTLS)
    } else {
        read_msr(msr::IA32_VMX_PROCBASED_CTLS)
    };

    let vmcs_primary_proc_based_exec_ctrl = vmread(vmcs::control::PRIMARY_PROCBASED_EXEC_CONTROLS)?;
    let msr_vmx_primary_proc_based_exec_ctrl_low =
        (msr_vmx_primary_proc_based_exec_ctrl & 0xFFFFFFFF) as u32;
    let msr_vmx_primary_proc_based_exec_ctrl_high =
        (msr_vmx_primary_proc_based_exec_ctrl >> 32) as u32;

    if !(vmcs_primary_proc_based_exec_ctrl & 0xFFFFFFFF) as u32
        & msr_vmx_primary_proc_based_exec_ctrl_low
        != 0
    {
        return Err(
            "VMCS Primary processor-based execution controls field: IA32_VMX_PROCBASED_CTRLS low bits not set",
        );
    }
    if (vmcs_primary_proc_based_exec_ctrl >> 32) as u32 & !msr_vmx_primary_proc_based_exec_ctrl_high
        != 0
    {
        return Err(
            "VMCS Primary processor-based execution controls field: IA32_VMX_PROCBASED_CTRLS high bits not zero",
        );
    }

    Ok(())
}

fn check_secondary_proc_based_exec_ctrl(vmx_true_ctrl: bool) -> Result<(), &'static str> {
    let msr_vmx_secondary_proc_based_exec_ctrl = if vmx_true_ctrl {
        read_msr(msr::IA32_VMX_PROCBASED_CTLS2)
    } else {
        0
    };

    let vmcs_secondary_proc_based_exec_ctrl =
        vmread(vmcs::control::SECONDARY_PROCBASED_EXEC_CONTROLS)?;
    let msr_vmx_secondary_proc_based_exec_ctrl_low =
        (msr_vmx_secondary_proc_based_exec_ctrl & 0xFFFFFFFF) as u32;
    let msr_vmx_secondary_proc_based_exec_ctrl_high =
        (msr_vmx_secondary_proc_based_exec_ctrl >> 32) as u32;

    if !(vmcs_secondary_proc_based_exec_ctrl & 0xFFFFFFFF) as u32
        & msr_vmx_secondary_proc_based_exec_ctrl_low
        != 0
    {
        return Err(
            "VMCS Secondary processor-based execution controls field: IA32_VMX_PROCBASED_CTRLS2 low bits not set",
        );
    }
    if (vmcs_secondary_proc_based_exec_ctrl >> 32) as u32
        & !msr_vmx_secondary_proc_based_exec_ctrl_high
        != 0
    {
        return Err(
            "VMCS Secondary processor-based execution controls field: IA32_VMX_PROCBASED_CTRLS2 high bits not zero",
        );
    }

    Ok(())
}
