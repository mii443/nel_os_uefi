use core::arch::asm;

use x86_64::{
    registers::control::{Cr0, Cr4, Cr4Flags},
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::{
    error,
    vmm::x86_64::{
        common::{read_msr, write_msr},
        intel::vmx_capture_status,
    },
};

#[repr(C, align(4096))]
pub struct Vmxon {
    pub frame: PhysFrame,
}

impl Vmxon {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate frame for VMXON")?;

        let mut vmxon = Vmxon { frame };
        vmxon.init();

        Ok(vmxon)
    }

    fn init(&mut self) {
        let revision_id = read_msr(0x480) as u32;
        let vmxon_region = self.frame.start_address().as_u64();

        unsafe {
            core::ptr::write_volatile(vmxon_region as *mut u32, revision_id);
        }
    }

    pub fn activate(&mut self) -> Result<(), &'static str> {
        Self::setup_vmxon();

        if !self.check_requirements() {
            return Err("VMX requirements not met");
        }

        let vmxon_region = self.frame.start_address().as_u64();
        unsafe {
            asm!(
                "vmxon ({})",
                in(reg) &vmxon_region,
                options(att_syntax)
            );
            vmx_capture_status()?;
        }

        Ok(())
    }

    fn setup_vmxon() {
        Self::enable_vmx_operation();
        Self::adjust_feature_control_msr();
        Self::set_cr0_bits();
    }

    fn enable_vmx_operation() {
        unsafe {
            Cr4::write_raw(Cr4::read_raw() | Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS.bits());
        }
    }

    fn adjust_feature_control_msr() {
        const VMX_LOCK_BIT: u64 = 1 << 0;
        const VMXON_OUTSIDE_SMX: u64 = 1 << 2;

        let ia32_feature_control = read_msr(0x3a);

        if ia32_feature_control & VMX_LOCK_BIT == 0 {
            write_msr(
                0x3a,
                ia32_feature_control | VMXON_OUTSIDE_SMX | VMX_LOCK_BIT,
            );
        }
    }

    fn set_cr0_bits() {
        let ia32_vmx_cr0_fixed0 = read_msr(0x486);
        let ia32_vmx_cr0_fixed1 = read_msr(0x487);

        let mut cr0 = Cr0::read_raw();

        cr0 |= ia32_vmx_cr0_fixed0;
        cr0 &= ia32_vmx_cr0_fixed1;

        unsafe { Cr0::write_raw(cr0) };
    }

    fn check_requirements(&mut self) -> bool {
        let cr4 = Cr4::read();
        if !cr4.contains(Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS) {
            error!("VMX is not enabled in CR4");
            return false;
        }

        let ia32_feature_control = read_msr(0x3a);
        if ia32_feature_control & (1 << 2) == 0 {
            error!("VMX operation not enabled outside of SMX");
            return false;
        }

        let ia32_vmx_cr0_fixed0 = read_msr(0x486);
        let ia32_vmx_cr0_fixed1 = read_msr(0x487);
        let cr0 = Cr0::read_raw();
        if cr0 & ia32_vmx_cr0_fixed0 != ia32_vmx_cr0_fixed0 {
            error!("CR0 does not meet VMX requirements");
            return false;
        }
        if cr0 & !ia32_vmx_cr0_fixed1 != 0 {
            error!("CR0 does not meet VMX requirements");
            return false;
        }

        let ia32_vmx_cr4_fixed0 = read_msr(0x488);
        let ia32_vmx_cr4_fixed1 = read_msr(0x489);
        let cr4 = Cr4::read_raw();
        if cr4 & ia32_vmx_cr4_fixed0 != ia32_vmx_cr4_fixed0 {
            error!("CR4 does not meet VMX requirements");
            return false;
        }
        if cr4 & !ia32_vmx_cr4_fixed1 != 0 {
            error!("CR4 does not meet VMX requirements");
            return false;
        }

        let vmxon_region = self.frame.start_address().as_u64();
        if vmxon_region & 0xfff != 0 {
            error!("VMXON region is not aligned to 4KiB");
            return false;
        }

        true
    }
}
