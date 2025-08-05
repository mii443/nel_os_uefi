use raw_cpuid::cpuid;
use x86_64::structures::paging::{FrameAllocator, Size4KiB};

use crate::{
    error, info,
    vmm::{x86_64::common, VCpu},
};

pub struct AMDVCpu;

impl VCpu for AMDVCpu {
    fn run(&mut self) -> Result<(), &'static str> {
        info!("VCpu on AMD");

        Ok(())
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
