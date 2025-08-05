use raw_cpuid::cpuid;
use x86_64::{
    registers::control::Cr4Flags,
    structures::paging::{FrameAllocator, Size4KiB},
};

use crate::{
    info,
    vmm::{
        x86_64::{
            common::{self, read_msr},
            intel::{controls, vmcs, vmwrite, vmxon},
        },
        VCpu,
    },
};

pub struct IntelVCpu {
    activated: bool,
    vmxon: vmxon::Vmxon,
    vmcs: vmcs::Vmcs,
}

impl IntelVCpu {
    fn activate(&mut self) -> Result<(), &'static str> {
        let revision_id = common::read_msr(0x480) as u32;
        self.vmcs.write_revision_id(revision_id);
        self.vmcs.reset()?;
        controls::setup_exec_controls()?;
        controls::setup_entry_controls()?;
        controls::setup_exit_controls()?;
        Self::setup_host_state()?;

        Ok(())
    }

    fn setup_host_state() -> Result<(), &'static str> {
        use x86::{
            controlregs::*, dtables, dtables::DescriptorTablePointer, segmentation::*, vmx::vmcs,
        };
        vmwrite(vmcs::host::CR0, unsafe { cr0() }.bits() as u64)?;
        vmwrite(vmcs::host::CR3, unsafe { cr3() })?;
        vmwrite(
            vmcs::host::CR4,
            unsafe { cr4() }.bits() as u64 | Cr4Flags::OSXSAVE.bits(),
        )?;

        // TODO: set RIP to VMExit handler and stack

        vmwrite(vmcs::host::ES_SELECTOR, es().bits() as u64)?;
        vmwrite(vmcs::host::CS_SELECTOR, cs().bits() as u64)?;
        vmwrite(vmcs::host::SS_SELECTOR, ss().bits() as u64)?;
        vmwrite(vmcs::host::DS_SELECTOR, ds().bits() as u64)?;
        vmwrite(vmcs::host::FS_SELECTOR, fs().bits() as u64)?;
        vmwrite(vmcs::host::GS_SELECTOR, gs().bits() as u64)?;

        vmwrite(vmcs::host::FS_BASE, read_msr(x86::msr::IA32_FS_BASE))?;
        vmwrite(vmcs::host::GS_BASE, read_msr(x86::msr::IA32_GS_BASE))?;

        let tr = unsafe { x86::task::tr() };
        let mut gdtp = DescriptorTablePointer::<u64>::default();
        let mut idtp = DescriptorTablePointer::<u64>::default();
        unsafe {
            dtables::sgdt(&mut gdtp);
            dtables::sidt(&mut idtp);
        }
        vmwrite(vmcs::host::GDTR_BASE, gdtp.base as u64)?;
        vmwrite(vmcs::host::IDTR_BASE, idtp.base as u64)?;
        vmwrite(vmcs::host::TR_SELECTOR, tr.bits() as u64)?;
        vmwrite(vmcs::host::TR_BASE, 0)?;

        vmwrite(vmcs::host::IA32_EFER_FULL, read_msr(x86::msr::IA32_EFER))?;

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
