use x86::{msr, vmx::vmcs};

use crate::vmm::x86_64::{
    common::read_msr,
    intel::{
        self,
        vmcs::controls::{
            PinBasedVmExecutionControls, PrimaryProcessorBasedVmExecutionControls,
            SecondaryProcessorBasedVmExecutionControls,
        },
        vmread,
    },
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

    check_cr3_target()?;

    check_io_bitmap()?;
    check_msr_bitmap()?;

    check_nmi()?;

    check_vmcs_shadowing()?;
    check_ept_violation_exception_info()?;
    check_interrupt()?;

    check_ept()?;

    Ok(())
}

fn is_valid_ept_ptr(ept_ptr: u64) -> Result<(), &'static str> {
    let memory_type = ept_ptr & 0b111;
    if memory_type != 0 && memory_type != 6 {
        return Err("VMCS EPT pointer memory type is not valid (must be 0 or 6)");
    }

    let walk_length = (ept_ptr >> 3) & 0b111;
    if walk_length != 3 {
        return Err("VMCS EPT pointer walk length is not valid (must be 3)");
    }

    if ept_ptr & 0xf00 != 0 {
        return Err("VMCS EPT pointer reserved bits are not zero");
    }

    if !is_valid_phys_addr(ept_ptr) {
        return Err("VMCS EPT pointer is not a valid physical address");
    }

    Ok(())
}

fn check_ept() -> Result<(), &'static str> {
    let secondary_exec_ctrl = SecondaryProcessorBasedVmExecutionControls::read()?;

    if secondary_exec_ctrl.ept() {
        let ept_ptr = vmread(vmcs::control::EPTP_FULL)?;
        is_valid_ept_ptr(ept_ptr)?;
    } else {
        if secondary_exec_ctrl.unrestricted_guest() {
            return Err(
                "VMCS Secondary processor-based execution controls field: EPT is not set while unrestricted guest is set",
            );
        }
        if secondary_exec_ctrl.mode_based_control_ept() {
            return Err(
                "VMCS Secondary processor-based execution controls field: EPT is not set while mode-based control for EPT is set",
            );
        }
    }

    Ok(())
}

fn check_interrupt() -> Result<(), &'static str> {
    let primary_exec_ctrl = PrimaryProcessorBasedVmExecutionControls::read()?;
    let secondary_exec_ctrl = SecondaryProcessorBasedVmExecutionControls::read()?;
    let pin_ctrl = PinBasedVmExecutionControls::read()?;

    if primary_exec_ctrl.use_tpr_shadow() {
        let virtual_apic_page_addr = vmread(vmcs::control::VIRT_APIC_ADDR_FULL)?;
        if !is_valid_page_aligned_phys_addr(virtual_apic_page_addr) {
            return Err(
                "VMCS virtual APIC page address is not a valid page-aligned physical address",
            );
        }

        if !secondary_exec_ctrl.virtual_interrupt_delivery() {
            if !pin_ctrl.external_interrupt_exiting() {
                return Err(
                    "VMCS Pin-based execution controls field: External interrupt exiting is not set while virtual interrupt delivery is set",
                );
            }
        } else {
            // TODO
        }
    } else {
        if secondary_exec_ctrl.virtualize_x2apic_mode()
            || secondary_exec_ctrl.apic_register_virtualization()
            || secondary_exec_ctrl.virtual_interrupt_delivery()
        {
            return Err(
                "VMCS Primary processor-based execution controls field: Use TPR shadow is not set while virtualize x2APIC mode, APIC register virtualization, or virtual interrupt delivery is set",
            );
        }
    }

    // TODO

    Ok(())
}

fn check_ept_violation_exception_info() -> Result<(), &'static str> {
    let secondary_exec_ctrl = SecondaryProcessorBasedVmExecutionControls::read()?;

    if secondary_exec_ctrl.ept_violation() {
        let exception_info = vmread(vmcs::control::VIRT_EXCEPTION_INFO_ADDR_FULL)?;

        if is_valid_page_aligned_phys_addr(exception_info) {
            return Err("VMCS EPT violation exception info address is not a valid page-aligned physical address");
        }
    }

    Ok(())
}

fn check_vmcs_shadowing() -> Result<(), &'static str> {
    let secondary_exec_ctrl = SecondaryProcessorBasedVmExecutionControls::read()?;

    if !secondary_exec_ctrl.vmcs_shadowing() {
        return Ok(());
    }

    let vmcs_vmread_bitmap = vmread(vmcs::control::VMREAD_BITMAP_ADDR_FULL)?;
    let vmcs_vmwrite_bitmap = vmread(vmcs::control::VMWRITE_BITMAP_ADDR_FULL)?;

    if !is_valid_page_aligned_phys_addr(vmcs_vmread_bitmap) {
        return Err("VMCS VMREAD bitmap address is not a valid page-aligned physical address");
    }

    if !is_valid_page_aligned_phys_addr(vmcs_vmwrite_bitmap) {
        return Err("VMCS VMWRITE bitmap address is not a valid page-aligned physical address");
    }

    Ok(())
}

fn check_nmi() -> Result<(), &'static str> {
    let pin_ctrl = PinBasedVmExecutionControls::read()?;

    if !pin_ctrl.nmi_exiting() && pin_ctrl.virtual_nmi() {
        return Err(
            "VMCS Pin-based execution controls field: NMI exiting and virtual NMI are both set",
        );
    }

    let exec_ctrl = PrimaryProcessorBasedVmExecutionControls::read()?;

    if !pin_ctrl.virtual_nmi() && exec_ctrl.nmi_window() {
        return Err(
            "VMCS Pin-based execution controls field: Interrupt-window exiting and virtual NMI are both not set",
        );
    }

    Ok(())
}

fn is_valid_phys_addr(addr: u64) -> bool {
    (addr & !((1 << 40) - 1)) == 0
}

fn is_valid_page_aligned_phys_addr(addr: u64) -> bool {
    (addr & (!((1 << 40) - 1) | 0xfff)) == 0
}

fn check_cr3_target() -> Result<(), &'static str> {
    let vmcs_cr3_target_count = vmread(vmcs::control::CR3_TARGET_COUNT)?;

    if vmcs_cr3_target_count > 4 {
        return Err("VMCS CR3-target count field is greater than 4");
    }

    Ok(())
}

fn check_msr_bitmap() -> Result<(), &'static str> {
    let vmcs_msr_bitmap = vmread(vmcs::control::MSR_BITMAPS_ADDR_FULL)?;

    if !is_valid_page_aligned_phys_addr(vmcs_msr_bitmap) {
        return Err("VMCS MSR bitmap address is not a valid page-aligned physical address");
    }

    Ok(())
}

fn check_io_bitmap() -> Result<(), &'static str> {
    let vmcs_io_bitmap_a = vmread(vmcs::control::IO_BITMAP_A_ADDR_FULL)?;
    let vmcs_io_bitmap_b = vmread(vmcs::control::IO_BITMAP_B_ADDR_FULL)?;

    if !is_valid_page_aligned_phys_addr(vmcs_io_bitmap_a) {
        return Err("VMCS IO bitmap A address is not a valid page-aligned physical address");
    }

    if !is_valid_page_aligned_phys_addr(vmcs_io_bitmap_b) {
        return Err("VMCS IO bitmap B address is not a valid page-aligned physical address");
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
