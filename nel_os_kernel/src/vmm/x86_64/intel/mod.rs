mod controls;
mod vmcs;
mod vmxon;

use core::arch::asm;

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
    activated: bool,
    vmxon: vmxon::Vmxon,
    vmcs: vmcs::Vmcs,
}

impl IntelVCpu {
    fn activate(&mut self) -> Result<(), &'static str> {
        self.vmcs.reset();
        controls::setup_exec_controls()?;

        Ok(())
    }
}

impl VCpu for IntelVCpu {
    fn run(&mut self) -> Result<(), &'static str> {
        info!("VCpu on Intel");

        if !self.activated {
            self.activate()?;
            self.activated = true;
        }

        Ok(())
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

        let vmcs = vmcs::Vmcs::new(frame_allocator)?;

        Ok(IntelVCpu {
            activated: false,
            vmxon,
            vmcs,
        })
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

pub fn vmread(field: u32) -> Result<u64, &'static str> {
    let field: u64 = field.into();
    let value: u64;
    unsafe {
        asm!(
            "vmread {0}, {1}",
            in(reg) field,
            out(reg) value,
            options(att_syntax)
        )
    };
    vmx_capture_status()?;
    Ok(value)
}

pub fn vmwrite(field: u32, value: u64) -> Result<(), &'static str> {
    let field: u64 = field.into();
    unsafe {
        asm!(
            "vmwrite {1}, {0}",
            in(reg) field,
            in(reg) value,
            options(att_syntax)
        )
    };
    vmx_capture_status()
}
