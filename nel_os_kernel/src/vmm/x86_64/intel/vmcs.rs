use core::arch::asm;

use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

use crate::vmm::x86_64::intel::vmx_capture_status;

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
