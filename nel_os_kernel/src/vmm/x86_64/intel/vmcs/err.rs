use numeric_enum_macro::numeric_enum;

use crate::vmm::x86_64::intel::vmcs::VmcsReadOnlyData32;

numeric_enum! {
    #[repr(u32)]
    #[derive(Debug, Copy, Clone)]
    pub enum InstructionError {
        NOT_AVAILABLE = 0,
        VMCALL_IN_VMXROOT = 1,
        VMCLEAR_INVALID_PHYS = 2,
        VMCLEAR_VMXONPTR = 3,
        VMLAUNCH_NONCLEAR_VMCS = 4,
        VMRESUME_NONLAUNCHED_VMCS = 5,
        VMRESUME_AFTER_VMXOFF = 6,
        VMENTRY_INVALID_CTRL = 7,
        VMENTRY_INVALID_HOST_STATE = 8,
        VMPTRLD_INVALID_PHYS = 9,
        VMPTRLD_VMXONP = 10,
        VMPTRLD_INCORRECT_REV = 11,
        VMRW_UNSUPPORTED_COMPONENT = 12,
        VMW_RO_COMPONENT = 13,
        VMXON_IN_VMXROOT = 15,
        VMENTRY_INVALID_EXEC_CTRL = 16,
        VMENTRY_NONLAUNCHED_EXEC_CTRL = 17,
        VMENTRY_EXEC_VMCSPTR = 18,
        VMCALL_NONCLEAR_VMCS = 19,
        VMCALL_INVALID_EXITCTL = 20,
        VMCALL_INCORRECT_MSGREV = 22,
        VMXOFF_DUALMONITOR = 23,
        VMCALL_INVALID_SMM = 24,
        VMENTRY_INVALID_EXECCTRL = 25,
        VMENTRY_EVENTS_BLOCKED = 26,
        INVALID_INVEPT = 28,
    }
}

impl InstructionError {
    pub fn read() -> Result<Self, &'static str> {
        let err = VmcsReadOnlyData32::VM_INSTRUCTION_ERROR.read()?;

        InstructionError::try_from(err).map_err(|_| "Unknown instruction error")
    }
}
