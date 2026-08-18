use ::x86_64::structures::paging::{FrameAllocator, Size4KiB};
use alloc::boxed::Box;

use crate::{
    platform,
    vmm::x86_64::{amd::vcpu::AMDVCpu, intel::vcpu::IntelVCpu},
};

pub mod x86_64;

pub const MAX_VMS: usize = 4;
pub const VCPUS_PER_VM: usize = 1;
pub const DEFAULT_GUEST_MEMORY_MIB: u32 = 128;
pub const MIN_GUEST_MEMORY_MIB: u32 = 64;
pub const MAX_GUEST_MEMORY_MIB: u32 = 768;
pub const VCPU_TIME_SLICE_MILLIS: u64 = 4;

pub trait VCpu {
    fn new(
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
        vm_id: usize,
        guest_memory_size: u64,
    ) -> Result<Self, &'static str>
    where
        Self: Sized;

    fn is_supported() -> bool
    where
        Self: Sized;

    fn run(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str>;

    /// Allocates guest RAM and initializes the boot state without executing
    /// guest instructions.
    fn prepare(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str>;

    /// Returns the existing VCPU and guest RAM to its boot state. Implementors
    /// must retain already allocated guest-memory mappings.
    fn reset(&mut self) -> Result<(), &'static str>;

    /// Reports that another immediate entry would only poll a halted guest.
    /// The scheduler uses this to rotate early instead of busy-waiting for the
    /// guest's next virtual interrupt.
    fn is_idle(&self) -> bool {
        false
    }

    fn write_memory(&mut self, addr: u64, data: u8) -> Result<(), &'static str>;
    fn write_memory_ranged(
        &mut self,
        addr_start: u64,
        addr_end: u64,
        data: u8,
    ) -> Result<(), &'static str>;
    fn read_memory(&mut self, addr: u64) -> Result<u8, &'static str>;

    fn write_memory_slice(&mut self, addr: u64, data: &[u8]) -> Result<(), &'static str> {
        for (offset, &byte) in data.iter().enumerate() {
            self.write_memory(addr + offset as u64, byte)?;
        }
        Ok(())
    }

    fn get_guest_memory_size(&self) -> u64;
    fn get_allocated_guest_memory_size(&self) -> u64;
}

pub fn get_vcpu(
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    vm_id: usize,
    guest_memory_size: u64,
) -> Result<Box<dyn VCpu>, &'static str> {
    if vm_id >= MAX_VMS {
        return Err("VM ID is out of range");
    }
    if platform::is_amd() && AMDVCpu::is_supported() {
        Ok(Box::new(AMDVCpu::new(
            frame_allocator,
            vm_id,
            guest_memory_size,
        )?))
    } else if platform::is_intel() && IntelVCpu::is_supported() {
        Ok(Box::new(IntelVCpu::new(
            frame_allocator,
            vm_id,
            guest_memory_size,
        )?))
    } else {
        Err("Unsupported CPU architecture")
    }
}
