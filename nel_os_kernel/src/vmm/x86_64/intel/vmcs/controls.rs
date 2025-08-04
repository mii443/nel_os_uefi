use modular_bitfield::{bitfield, prelude::*};

use crate::vmm::x86_64::intel::vmcs;

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct PinBasedVmExecutionControls {
    pub external_interrupt_exiting: bool,
    reserved1: B1,
    pub interrupt_window_exiting: bool,
    pub nmi_exiting: bool,
    reserved2: B1,
    pub virtual_nmi: bool,
    pub activate_vmx_preemption_timer: bool,
    pub process_posted_interrupts: bool,
    reserved3: B24,
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
