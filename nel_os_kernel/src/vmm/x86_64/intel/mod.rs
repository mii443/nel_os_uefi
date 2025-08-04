use raw_cpuid::cpuid;

use crate::{
    info,
    vmm::{x86_64::common, VCpu},
};

pub struct IntelVCpu;

impl IntelVCpu {
    pub fn new() -> Self {
        let mut msr = common::read_msr(0x3a);
        if msr & (1 << 2) == 0 {
            msr |= 1 << 2;
            msr |= 1;
            common::write_msr(0x3a, msr);
        }

        let msr = common::read_msr(0x3a);
        if msr & (1 << 2) == 0 {
            panic!("VMX is not enabled in the BIOS");
        }

        IntelVCpu
    }
}

impl VCpu for IntelVCpu {
    fn run(&mut self) {
        info!("VCpu on Intel");
    }

    fn is_supported() -> bool
    where
        Self: Sized,
    {
        if cpuid!(0x1).ecx & (1 << 5) == 0 {
            info!("Intel CPU does not support VMX");
            return false;
        }

        let msr = common::read_msr(0x3a);
        if msr & (1 << 2) == 0 && msr & 1 != 0 {
            info!("VMX is not enabled in the BIOS");
            return false;
        }
        true
    }
}
