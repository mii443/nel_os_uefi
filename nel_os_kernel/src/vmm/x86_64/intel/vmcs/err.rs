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

    pub fn to_str(self) -> &'static str {
        match self {
            InstructionError::NOT_AVAILABLE => "Instruction not available",
            InstructionError::VMCALL_IN_VMXROOT => "VMCALL in VMX root operation",
            InstructionError::VMCLEAR_INVALID_PHYS => "Invalid physical address for VMCLEAR",
            InstructionError::VMCLEAR_VMXONPTR => "VMCLEAR with VMXON pointer",
            InstructionError::VMLAUNCH_NONCLEAR_VMCS => "VMLAUNCH with non-cleared VMCS",
            InstructionError::VMRESUME_NONLAUNCHED_VMCS => "VMRESUME with non-launched VMCS",
            InstructionError::VMRESUME_AFTER_VMXOFF => "VMRESUME after VMXOFF",
            InstructionError::VMENTRY_INVALID_CTRL => "Invalid control fields for VMENTRY",
            InstructionError::VMENTRY_INVALID_HOST_STATE => "Invalid host state for VMENTRY",
            InstructionError::VMPTRLD_INVALID_PHYS => "Invalid physical address for VMPTRLD",
            InstructionError::VMPTRLD_VMXONP => "VMPTRLD with VMXON pointer",
            InstructionError::VMPTRLD_INCORRECT_REV => "Incorrect revision identifier for VMPTRLD",
            InstructionError::VMRW_UNSUPPORTED_COMPONENT => "Unsupported component in VMRW",
            InstructionError::VMW_RO_COMPONENT => "Read-only component in VMWRITE",
            InstructionError::VMXON_IN_VMXROOT => "VMXON in VMX root operation",
            InstructionError::VMENTRY_INVALID_EXEC_CTRL => "Invalid execution controls for VMENTRY",
            InstructionError::VMENTRY_NONLAUNCHED_EXEC_CTRL => {
                "Non-launched execution controls for VMENTRY"
            }
            InstructionError::VMENTRY_EXEC_VMCSPTR => "Execution control VMCS pointer for VMENTRY",
            InstructionError::VMCALL_NONCLEAR_VMCS => "VMCALL with non-cleared VMCS",
            InstructionError::VMCALL_INVALID_EXITCTL => "Invalid exit control fields for VMCALL",
            InstructionError::VMCALL_INCORRECT_MSGREV => "Incorrect message revision for VMCALL",
            InstructionError::VMXOFF_DUALMONITOR => "VMXOFF in dual-monitor mode",
            InstructionError::VMCALL_INVALID_SMM => "Invalid SMM state for VMCALL",
            InstructionError::VMENTRY_INVALID_EXECCTRL => "Invalid execution controls for VMENTRY",
            InstructionError::VMENTRY_EVENTS_BLOCKED => "Events blocked during VMENTRY",
            InstructionError::INVALID_INVEPT => "Invalid INVEPT operation",
        }
    }
}
