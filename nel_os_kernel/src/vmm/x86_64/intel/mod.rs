mod vmxon;

use raw_cpuid::cpuid;

use crate::{
    info,
    vmm::{x86_64::common, VCpu},
};

pub struct IntelVCpu;

impl VCpu for IntelVCpu {
    fn run(&mut self) {
        info!("VCpu on Intel");
    }

    fn new() -> Result<Self, &'static str>
    where
        Self: Sized,
    {
        let mut msr = common::read_msr(0x3a);
        if msr & (1 << 2) == 0 {
            msr |= 1 << 2;
            msr |= 1;
            common::write_msr(0x3a, msr);
        }

        let msr = common::read_msr(0x3a);
        if msr & (1 << 2) == 0 {
            return Err("VMX is not enabled in the BIOS");
        }

        Ok(IntelVCpu)
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
