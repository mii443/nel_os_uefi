mod vmxon;

use raw_cpuid::cpuid;
use x86_64::{
    registers::rflags::{self, RFlags},
    structures::paging::{FrameAllocator, Size4KiB},
};

use crate::{
    info,
    vmm::{x86_64::common, VCpu},
};

pub struct IntelVCpu {
    vmxon: vmxon::Vmxon,
}

impl VCpu for IntelVCpu {
    fn run(&mut self) {
        info!("VCpu on Intel");
    }

    fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str>
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

        let mut vmxon = vmxon::Vmxon::new(frame_allocator)?;

        vmxon.activate()?;

        Ok(IntelVCpu { vmxon })
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

pub fn vmx_capture_status() -> Result<(), &'static str> {
    let flags = rflags::read();
    if flags.contains(RFlags::ZERO_FLAG) {
        Err("VM fail valid")
    } else if flags.contains(RFlags::CARRY_FLAG) {
        Err("VM fail invalid")
    } else {
        Ok(())
    }
}
