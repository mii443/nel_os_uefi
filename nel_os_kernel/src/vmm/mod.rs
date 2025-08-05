use ::x86_64::structures::paging::{FrameAllocator, Size4KiB};
use alloc::boxed::Box;

use crate::{
    platform,
    vmm::x86_64::{amd::vcpu::AMDVCpu, intel::vcpu::IntelVCpu},
};

pub mod x86_64;

pub trait VCpu {
    fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str>
    where
        Self: Sized;
    fn is_supported() -> bool
    where
        Self: Sized;
    fn run(&mut self) -> Result<(), &'static str>;
}

pub fn get_vcpu(
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<Box<dyn VCpu>, &'static str> {
    if platform::is_amd() && AMDVCpu::is_supported() {
        Ok(Box::new(AMDVCpu::new(frame_allocator)?))
    } else if platform::is_intel() && IntelVCpu::is_supported() {
        Ok(Box::new(IntelVCpu::new(frame_allocator)?))
    } else {
        Err("Unsupported CPU architecture")
    }
}
