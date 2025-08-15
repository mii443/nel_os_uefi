use raw_cpuid::cpuid;
use x86_64::structures::paging::{FrameAllocator, Size4KiB};

use crate::{
    error, info,
    vmm::{x86_64::common, VCpu},
};

pub struct AMDVCpu;

impl VCpu for AMDVCpu {
    fn run(
        &mut self,
        _frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        info!("VCpu on AMD");

        Ok(())
    }

    fn write_memory(&mut self, _addr: u64, _data: u8) -> Result<(), &'static str> {
        unimplemented!("AMDVCpu::write_memory is not implemented yet");
    }

    fn write_memory_ranged(
        &mut self,
        _addr_start: u64,
        _addr_end: u64,
        _data: u8,
    ) -> Result<(), &'static str> {
        unimplemented!("AMDVCpu::write_memory_ranged is not implemented yet");
    }

    fn read_memory(&mut self, _addr: u64) -> Result<u8, &'static str> {
        unimplemented!("AMDVCpu::read_memory is not implemented yet");
    }

    fn get_guest_memory_size(&self) -> u64 {
        unimplemented!("AMDVCpu::get_guest_memory_size is not implemented yet")
    }

    fn new(_frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str>
    where
        Self: Sized,
    {
        let mut efer = common::read_msr(0xc000_0080);
        efer |= 1 << 12;
        common::write_msr(0xc000_0080, efer);

        Ok(AMDVCpu)
    }

    fn is_supported() -> bool
    where
        Self: Sized,
    {
        if cpuid!(0x8000_0001).ecx & (1 << 2) == 0 {
            error!("SVM not supported by CPU");
            return false;
        }

        if common::read_msr(0xc001_0114) & (1 << 4) != 0 {
            error!("SVM disabled by BIOS");
            return false;
        }

        true
    }
}
