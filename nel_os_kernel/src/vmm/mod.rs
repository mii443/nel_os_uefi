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

    fn run(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str>;

    fn write_memory(&mut self, addr: u64, data: u8) -> Result<(), &'static str>;
    fn write_memory_ranged(
        &mut self,
        addr_start: u64,
        addr_end: u64,
        data: u8,
    ) -> Result<(), &'static str>;
    fn read_memory(&mut self, addr: u64) -> Result<u8, &'static str>;

    fn get_guest_memory_size(&self) -> u64;
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
