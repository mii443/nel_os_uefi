use raw_cpuid::cpuid;

use crate::{
    error, info,
    vmm::{x86_64::common, VCpu},
};

pub struct AMDVCpu;

impl AMDVCpu {
    pub fn new() -> Self {
        let mut efer = common::read_msr(0xc000_0080);
        efer |= 1 << 12;
        common::write_msr(0xc000_0080, efer);

        AMDVCpu
    }
}

impl VCpu for AMDVCpu {
    fn run(&mut self) {
        info!("VCpu on AMD");
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
