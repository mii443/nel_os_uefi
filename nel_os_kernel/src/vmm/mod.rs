use ::x86_64::structures::paging::{FrameAllocator, Size4KiB};
use alloc::boxed::Box;

use crate::{
    memory::bitmap::BitmapMemoryTable,
    network::PassthroughDescriptor,
    platform,
    storage::VirtioBlock,
    vmm::x86_64::{amd::vcpu::AMDVCpu, intel::vcpu::IntelVCpu},
};

pub mod x86_64;

pub const VCPUS_PER_VM: usize = 1;
pub const DEFAULT_GUEST_MEMORY_MIB: u32 = 256;
// The bundled EFI Linux image needs enough contiguous RAM for the compressed
// image, its decompressed kernel, initrd, and OVMF allocations at the same
// time. At 128 MiB the EFI stub fails with EFI_OUT_OF_RESOURCES.
pub const MIN_GUEST_MEMORY_MIB: u32 = 256;
pub const MAX_GUEST_MEMORY_MIB: u32 = 4 * 1024;
pub const VCPU_TIME_SLICE_MILLIS: u64 = 4;
pub const MAX_VCPU_HEAP_BYTES: usize = {
    let amd = core::mem::size_of::<AMDVCpu>();
    let intel = core::mem::size_of::<IntelVCpu>();
    if amd > intel { amd } else { intel }
};

pub trait VCpu {
    fn new(
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
        hardware_vcpu_id: usize,
        guest_memory_size: u64,
        passthrough: Option<PassthroughDescriptor>,
    ) -> Result<Self, &'static str>
    where
        Self: Sized;

    fn is_supported() -> bool
    where
        Self: Sized;

    fn run(
        &mut self,
        frame_allocator: &mut BitmapMemoryTable,
        block: Option<&mut VirtioBlock>,
    ) -> Result<(), &'static str>;

    /// Allocates guest RAM and initializes the boot state without executing
    /// guest instructions.
    fn prepare(&mut self, frame_allocator: &mut BitmapMemoryTable) -> Result<(), &'static str>;

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

    /// Returns the approximate guest working set. This is derived from the
    /// accessed bits in the second-level page tables and therefore excludes
    /// backing pages that the guest has never touched.
    fn get_used_guest_memory_size(&self) -> u64;
}

pub fn get_vcpu(
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    hardware_vcpu_id: usize,
    guest_memory_size: u64,
    passthrough: Option<PassthroughDescriptor>,
) -> Result<Box<dyn VCpu>, &'static str> {
    if platform::is_amd() && AMDVCpu::is_supported() {
        Box::try_new(AMDVCpu::new(
            frame_allocator,
            hardware_vcpu_id,
            guest_memory_size,
            passthrough,
        )?)
        .map(|vcpu| -> Box<dyn VCpu> { vcpu })
        .map_err(|_| "Management heap cannot allocate another AMD VCPU")
    } else if platform::is_intel() && IntelVCpu::is_supported() {
        Box::try_new(IntelVCpu::new(
            frame_allocator,
            hardware_vcpu_id,
            guest_memory_size,
            passthrough,
        )?)
        .map(|vcpu| -> Box<dyn VCpu> { vcpu })
        .map_err(|_| "Management heap cannot allocate another Intel VCPU")
    } else {
        Err("Unsupported CPU architecture")
    }
}
