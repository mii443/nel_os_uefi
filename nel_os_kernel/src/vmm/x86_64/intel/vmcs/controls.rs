#![allow(non_snake_case)]

use modular_bitfield::{bitfield, prelude::*};

use crate::vmm::x86_64::intel::vmcs;

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct PinBasedVmExecutionControls {
    pub external_interrupt_exiting: bool,
    _reserved1: B1,
    pub interrupt_window_exiting: bool,
    pub nmi_exiting: bool,
    _reserved2: B1,
    pub virtual_nmi: bool,
    pub activate_vmx_preemption_timer: bool,
    pub process_posted_interrupts: bool,
    _reserved3: B24,
}

impl PinBasedVmExecutionControls {
    pub fn read() -> Result<Self, &'static str> {
        vmcs::VmcsControl32::PIN_BASED_VM_EXECUTION_CONTROLS
            .read()
            .map(|value| PinBasedVmExecutionControls::from(value))
            .map_err(|_| "Failed to read Pin-Based VM Execution Controls")
    }

    pub fn write(&self) -> Result<(), &'static str> {
        vmcs::VmcsControl32::PIN_BASED_VM_EXECUTION_CONTROLS.write(u32::from(*self))
    }
}

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct PrimaryProcessorBasedVmExecutionControls {
    _reserved1: B2,
    pub interrupt_window: bool,
    pub tsc_offsetting: bool,
    _reserved2: B3,
    pub hlt: bool,
    _reserved3: B1,
    pub invlpg: bool,
    pub mwait: bool,
    pub rdpmc: bool,
    pub rdtsc: bool,
    _reserved4: B2,
    pub cr3load: bool,
    pub cr3store: bool,
    pub activate_teritary_controls: bool,
    _reserved5: B1,
    pub cr8load: bool,
    pub cr8store: bool,
    pub use_tpr_shadow: bool,
    pub nmi_window: bool,
    pub mov_dr: bool,
    pub unconditional_io: bool,
    pub use_io_bitmap: bool,
    _reserved6: B1,
    pub monitor_trap: bool,
    pub use_msr_bitmap: bool,
    pub monitor: bool,
    pub pause: bool,
    pub activate_secondary_controls: bool,
}

impl PrimaryProcessorBasedVmExecutionControls {
    pub fn read() -> Result<Self, &'static str> {
        vmcs::VmcsControl32::PRIMARY_PROCESSOR_BASED_VM_EXECUTION_CONTROLS
            .read()
            .map(|value| PrimaryProcessorBasedVmExecutionControls::from(value))
            .map_err(|_| "Failed to read Primary Processor-Based VM Execution Controls")
    }

    pub fn write(&self) -> Result<(), &'static str> {
        vmcs::VmcsControl32::PRIMARY_PROCESSOR_BASED_VM_EXECUTION_CONTROLS.write(u32::from(*self))
    }
}

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct SecondaryProcessorBasedVmExecutionControls {
    pub virtualize_apic_accesses: bool,
    pub ept: bool,
    pub descriptor_table: bool,
    pub rdtscp: bool,
    pub virtualize_x2apic_mode: bool,
    pub vpid: bool,
    pub wbinvd: bool,
    pub unrestricted_guest: bool,
    pub apic_register_virtualization: bool,
    pub virtual_interrupt_delivery: bool,
    pub pause_loop: bool,
    pub rdrand: bool,
    pub enable_invpcid: bool,
    pub enable_vmfunc: bool,
    pub vmcs_shadowing: bool,
    pub enable_encls: bool,
    pub rdseed: bool,
    pub enable_pml: bool,
    pub ept_violation: bool,
    pub conceal_vmx_from_pt: bool,
    pub enable_xsaves_xrstors: bool,
    pub pasid_translation: bool,
    pub mode_based_control_ept: bool,
    pub subpage_write_eptr: bool,
    pub pt_guest_pa: bool,
    pub tsc_scaling: bool,
    pub enable_user_wait_pause: bool,
    pub enable_pconfig: bool,
    pub enable_enclv: bool,
    pub vmm_buslock_detect: bool,
    pub instruction_timeout: bool,
    _reserved: B1,
}

impl SecondaryProcessorBasedVmExecutionControls {
    pub fn read() -> Result<Self, &'static str> {
        vmcs::VmcsControl32::SECONDARY_PROCESSOR_BASED_VM_EXECUTION_CONTROLS
            .read()
            .map(|value| SecondaryProcessorBasedVmExecutionControls::from(value))
            .map_err(|_| "Failed to read Secondary Processor-Based VM Execution Controls")
    }

    pub fn write(&self) -> Result<(), &'static str> {
        vmcs::VmcsControl32::SECONDARY_PROCESSOR_BASED_VM_EXECUTION_CONTROLS.write(u32::from(*self))
    }
}

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct EntryControls {
    _reserved1: B2,
    pub load_debug_controls: bool,
    _reserved2: B6,
    pub ia32e_mode_guest: bool,
    pub entry_smm: bool,
    pub deactivate_dualmonitor: bool,
    _reserved3: B1,
    pub load_perf_global_ctrl: bool,
    pub load_ia32_pat: bool,
    pub load_ia32_efer: bool,
    pub load_ia32_bndcfgs: bool,
    pub conceal_vmx_from_pt: bool,
    pub load_rtit_ctl: bool,
    pub load_uinv: bool,
    pub load_cet_state: bool,
    pub load_guest_lbr_ctl: bool,
    pub load_pkrs: bool,
    _reserved4: B9,
}

impl EntryControls {
    pub fn read() -> Result<Self, &'static str> {
        vmcs::VmcsControl32::VM_ENTRY_CONTROLS
            .read()
            .map(|value| EntryControls::from(value))
            .map_err(|_| "Failed to read VM Entry Controls")
    }

    pub fn write(&self) -> Result<(), &'static str> {
        vmcs::VmcsControl32::VM_ENTRY_CONTROLS.write(u32::from(*self))
    }
}

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct PrimaryExitControls {
    _reserved1: B2,
    pub save_debug: bool,
    _reserved2: B6,
    pub host_addr_space_size: bool,
    _reserved3: B3,
    pub load_perf_global_ctrl: bool,
    _reserved4: B1,
    pub ack_interrupt_onexit: bool,
    _reserved5: B2,
    pub save_ia32_pat: bool,
    pub load_ia32_pat: bool,
    pub save_ia32_efer: bool,
    pub load_ia32_efer: bool,
    pub save_vmx_preemption_timer: bool,
    pub clear_ia32_bndcfgs: bool,
    pub conceal_vmx_from_pt: bool,
    pub clear_ia32_rtit_ctl: bool,
    pub clear_ia32_lbr_ctl: bool,
    pub clear_uinv: bool,
    pub load_cet_state: bool,
    pub load_pkrs: bool,
    pub save_perf_global_ctl: bool,
    pub activate_secondary_controls: bool,
}

impl PrimaryExitControls {
    pub fn read() -> Result<Self, &'static str> {
        vmcs::VmcsControl32::PRIMARY_VM_EXIT_CONTROLS
            .read()
            .map(|value| PrimaryExitControls::from(value))
            .map_err(|_| "Failed to read Primary VM Exit Controls")
    }

    pub fn write(&self) -> Result<(), &'static str> {
        vmcs::VmcsControl32::PRIMARY_VM_EXIT_CONTROLS.write(u32::from(*self))
    }
}
