use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

#[repr(C, align(4096))]
pub struct Vmxon {
    frame: PhysFrame,
}

impl Vmxon {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate frame for VMXON")?;

        Ok(Vmxon { frame })
    }
}
