use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

pub struct Vmcb {
    pub frame: PhysFrame,
}

impl Vmcb {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate VMCB frame")?;
        Ok(Vmcb { frame })
    }
}
