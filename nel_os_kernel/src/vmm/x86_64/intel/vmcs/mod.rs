#![allow(non_camel_case_types)]

pub mod controls;
pub mod err;
pub mod exit_reason;

use core::arch::asm;

use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

use crate::vmm::x86_64::intel::vmx_capture_status;

macro_rules! vmcs_read {
    ($field_enum: ident, u64) => {
        impl $field_enum {
            pub fn read(self) -> Result<u64, &'static str> {
                crate::vmm::x86_64::intel::vmread(self as u32)
            }
        }
    };
    ($field_enum: ident, $ux: ty) => {
        impl $field_enum {
            pub fn read(self) -> Result<$ux, &'static str> {
                crate::vmm::x86_64::intel::vmread(self as u32).map(|v| v as $ux)
            }
        }
    };
}

macro_rules! vmcs_write {
    ($field_enum: ident, u64) => {
        impl $field_enum {
            pub fn write(self, value: u64) -> Result<(), &'static str> {
                crate::vmm::x86_64::intel::vmwrite(self as u32, value)
            }
        }
    };
    ($field_enum: ident, $ux: ty) => {
        impl $field_enum {
            pub fn write(self, value: $ux) -> Result<(), &'static str> {
                crate::vmm::x86_64::intel::vmwrite(self as u32, value as u64)
            }
        }
    };
}

pub struct Vmcs {
    pub frame: PhysFrame,
}

impl Vmcs {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate VMCS frame")?;
        Ok(Vmcs { frame })
    }

    pub fn reset(&mut self) -> Result<(), &'static str> {
        let vmcs_addr = self.get_vmcs_addr();

        unsafe {
            asm!(
                "vmclear ({})",
                in(reg) &vmcs_addr,
                options(att_syntax)
            );
            vmx_capture_status()?;
            asm!(
                "vmptrld ({})",
                in(reg) &vmcs_addr,
                options(att_syntax)
            );
            vmx_capture_status()
        }
    }

    pub fn write_revision_id(&mut self, revision_id: u32) {
        let vmcs_addr = self.get_vmcs_addr();

        unsafe {
            core::ptr::write_volatile(vmcs_addr as *mut u32, revision_id);
        }
    }

    #[inline]
    fn get_vmcs_addr(&self) -> u64 {
        self.frame.start_address().as_u64()
    }
}

pub enum VmcsControl32 {
    PIN_BASED_VM_EXECUTION_CONTROLS = 0x00004000,
    PRIMARY_PROCESSOR_BASED_VM_EXECUTION_CONTROLS = 0x00004002,
    EXCEPTION_BITMAP = 0x00004004,
    PAGE_FAULT_ERROR_CODE_MASK = 0x00004006,
    PAGE_FAULT_ERROR_CODE_MATCH = 0x00004008,
    CR3_TARGET_COUNT = 0x0000400A,
    PRIMARY_VM_EXIT_CONTROLS = 0x0000400C,
    VM_EXIT_MSR_STORE_COUNT = 0x0000400E,
    VM_EXIT_MSR_LOAD_COUNT = 0x00004010,
    VM_ENTRY_CONTROLS = 0x00004012,
    VM_ENTRY_MSR_LOAD_COUNT = 0x00004014,
    VM_ENTRY_INTERRUPTION_INFORMATION_FIELD = 0x00004016,
    VM_ENTRY_EXCEPTION_ERROR_CODE = 0x00004018,
    VM_ENTRY_INSTRUCTION_LENGTH = 0x0000401A,
    TPR_THRESHOLD = 0x0000401C,
    SECONDARY_PROCESSOR_BASED_VM_EXECUTION_CONTROLS = 0x0000401E,
    PLE_GAP = 0x00004020,
    PLE_WINDOW = 0x00004022,
    INSTRUCTION_TIMEOUT_CONTROL = 0x00004024,
}
vmcs_read!(VmcsControl32, u32);
vmcs_write!(VmcsControl32, u32);

pub enum VmcsReadOnlyData32 {
    VM_INSTRUCTION_ERROR = 0x00004400,
    VM_EXIT_REASON = 0x00004402,
    VM_EXIT_INTERRUPTION_INFORMATION_FIELD = 0x00004404,
    VM_EXIT_INTERRUPTION_ERROR_CODE = 0x00004406,
    IDT_VECTORING_INFORMATION_FIELD = 0x00004408,
    IDT_VECTORING_ERROR_CODE = 0x0000440A,
    VM_EXIT_INSTRUCTION_LENGTH = 0x0000440C,
    VM_EXIT_INSTRUCTION_INFO = 0x0000440E,
}
vmcs_read!(VmcsReadOnlyData32, u32);
