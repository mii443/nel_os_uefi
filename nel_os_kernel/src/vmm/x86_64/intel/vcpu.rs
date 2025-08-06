use core::arch::naked_asm;

use raw_cpuid::cpuid;
use x86_64::{
    registers::control::Cr4Flags,
    structures::paging::{frame, FrameAllocator, Size4KiB},
    VirtAddr,
};

use crate::{
    info,
    vmm::{
        x86_64::{
            common::{self, read_msr},
            intel::{
                controls, ept,
                register::GuestRegisters,
                vmcs::{
                    self,
                    err::InstructionError,
                    exit_reason::VmxExitReason,
                    segment::{DescriptorType, Granularity, SegmentRights},
                },
                vmread, vmwrite, vmxon,
            },
        },
        VCpu,
    },
};

const TEMP_STACK_SIZE: usize = 4096;
static mut TEMP_STACK: [u8; TEMP_STACK_SIZE + 0x10] = [0; TEMP_STACK_SIZE + 0x10];

#[repr(C)]
pub struct IntelVCpu {
    pub launch_done: bool,
    pub guest_registers: GuestRegisters,
    activated: bool,
    vmxon: vmxon::Vmxon,
    vmcs: vmcs::Vmcs,
    ept: ept::EPT,
    eptp: ept::EPTP,
}

impl IntelVCpu {
    #[unsafe(naked)]
    unsafe extern "C" fn test_guest_code() -> ! {
        naked_asm!("2: hlt; jmp 2b");
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn intel_set_host_stack(rsp: u64) {
        vmwrite(x86::vmx::vmcs::host::RSP, rsp).unwrap();
    }

    fn vmexit_handler(&mut self) -> Result<(), &'static str> {
        use x86::vmx::vmcs;
        let exit_reason_raw = vmread(vmcs::ro::EXIT_REASON)? as u32;

        if exit_reason_raw & (1 << 31) != 0 {
            let reason = exit_reason_raw & 0xFF;
            info!("VMEntry failure");
            match reason {
                33 => {
                    info!("    Reason: invalid guest state");
                }
                _ => {
                    info!("    Reason: unknown ({})", reason);
                }
            }
        } else {
            let basic_reason = (exit_reason_raw & 0xFFFF) as u16;
            let exit_reason: VmxExitReason = basic_reason.try_into().unwrap();

            match exit_reason {
                VmxExitReason::HLT => {
                    info!("VM hlt");
                }
                _ => {
                    info!("VM exit reason: {:?}", exit_reason);
                }
            }
        }

        Ok(())
    }

    fn vmentry(&mut self) -> Result<(), InstructionError> {
        info!("VMEntry");
        let success = {
            let result: u16;
            unsafe {
                result = crate::vmm::x86_64::intel::asm::asm_vm_entry(self as *mut _);
            };
            result == 0
        };
        info!("VMEntry result: {}", success);

        if !self.launch_done && success {
            self.launch_done = true;
        }

        if !success {
            let error = InstructionError::read().unwrap();
            if error as u32 != 0 {
                return Err(error);
            }
        }

        Ok(())
    }

    fn activate(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        let revision_id = common::read_msr(0x480) as u32;
        self.vmcs.write_revision_id(revision_id);
        self.vmcs.reset()?;
        controls::setup_exec_controls()?;
        controls::setup_entry_controls()?;
        controls::setup_exit_controls()?;
        Self::setup_host_state()?;
        Self::setup_guest_state()?;

        self.init_guest_memory(frame_allocator)?;

        Ok(())
    }

    fn init_guest_memory(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        let mut pages = 1000;
        let mut gpa = 0;

        while pages > 0 {
            let frame = frame_allocator.allocate_frame().ok_or("No free frames")?;
            let hpa = frame.start_address().as_u64();

            self.ept.map_2m(gpa, hpa, frame_allocator)?;
            gpa += (4 * 1024) << 9;
            pages -= 1;
        }

        let guest_ptr = Self::test_guest_code as u64;
        let guest_addr = self.ept.get_phys_addr(0).unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(guest_ptr as *const u8, guest_addr as *mut u8, 200);
        }

        let eptp = ept::EPTP::init(&self.ept.root_table);
        vmwrite(x86::vmx::vmcs::control::EPTP_FULL, u64::from(eptp))?;

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
            unsafe { cr4() }.bits() as u64, /* | Cr4Flags::OSXSAVE.bits()*/
        )?;

        vmwrite(
            vmcs::host::RIP,
            crate::vmm::x86_64::intel::asm::asm_vmexit_handler as u64,
        )?;
        vmwrite(
            vmcs::host::RSP,
            VirtAddr::from_ptr(&raw mut TEMP_STACK).as_u64() + TEMP_STACK_SIZE as u64,
        )?;

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

    fn setup_guest_state() -> Result<(), &'static str> {
        use x86::{controlregs::*, vmx::vmcs};
        let cr0 = Cr0::empty()
            | Cr0::CR0_PROTECTED_MODE
            | Cr0::CR0_NUMERIC_ERROR & !Cr0::CR0_ENABLE_PAGING;
        vmwrite(vmcs::guest::CR0, cr0.bits() as u64)?;
        vmwrite(vmcs::guest::CR3, unsafe { cr3() })?;
        vmwrite(
            vmcs::guest::CR4,
            unsafe { cr4() }.bits() as u64, /*vmread(vmcs::guest::CR4)? & !Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS.bits()*/
        )?;

        vmwrite(vmcs::guest::CS_BASE, 0)?;
        vmwrite(vmcs::guest::SS_BASE, 0)?;
        vmwrite(vmcs::guest::DS_BASE, 0)?;
        vmwrite(vmcs::guest::ES_BASE, 0)?;
        vmwrite(vmcs::guest::TR_BASE, 0)?;
        vmwrite(vmcs::guest::GDTR_BASE, 0)?;
        vmwrite(vmcs::guest::IDTR_BASE, 0)?;
        vmwrite(vmcs::guest::LDTR_BASE, 0xDEAD00)?;

        vmwrite(vmcs::guest::CS_LIMIT, 0xffff)?;
        vmwrite(vmcs::guest::SS_LIMIT, 0xffff)?;
        vmwrite(vmcs::guest::DS_LIMIT, 0xffff)?;
        vmwrite(vmcs::guest::ES_LIMIT, 0xffff)?;
        vmwrite(vmcs::guest::FS_LIMIT, 0xffff)?;
        vmwrite(vmcs::guest::GS_LIMIT, 0xffff)?;
        vmwrite(vmcs::guest::TR_LIMIT, 0)?;
        vmwrite(vmcs::guest::GDTR_LIMIT, 0)?;
        vmwrite(vmcs::guest::IDTR_LIMIT, 0)?;
        vmwrite(vmcs::guest::LDTR_LIMIT, 0)?;

        let cs_right = SegmentRights::default()
            .with_rw(true)
            .with_dc(false)
            .with_executable(true)
            .with_desc_type(DescriptorType::Code)
            .with_dpl(0)
            .with_granularity(Granularity::KByte)
            .with_long(true)
            .with_db(false);

        let ds_right = SegmentRights::default()
            .with_rw(true)
            .with_dc(false)
            .with_executable(false)
            .with_desc_type(DescriptorType::Code)
            .with_dpl(0)
            .with_granularity(Granularity::KByte)
            .with_long(false)
            .with_db(true);

        let tr_right = SegmentRights::default()
            .with_rw(true)
            .with_dc(false)
            .with_executable(true)
            .with_desc_type(DescriptorType::System)
            .with_dpl(0)
            .with_granularity(Granularity::Byte)
            .with_long(false)
            .with_db(false);

        let ldtr_right = SegmentRights::default()
            .with_accessed(false)
            .with_rw(true)
            .with_dc(false)
            .with_executable(false)
            .with_desc_type(DescriptorType::System)
            .with_dpl(0)
            .with_granularity(Granularity::Byte)
            .with_long(false)
            .with_db(false);

        vmwrite(vmcs::guest::CS_ACCESS_RIGHTS, u32::from(cs_right) as u64)?;
        vmwrite(vmcs::guest::SS_ACCESS_RIGHTS, u32::from(ds_right) as u64)?;
        vmwrite(vmcs::guest::DS_ACCESS_RIGHTS, u32::from(ds_right) as u64)?;
        vmwrite(vmcs::guest::ES_ACCESS_RIGHTS, u32::from(ds_right) as u64)?;
        vmwrite(vmcs::guest::FS_ACCESS_RIGHTS, u32::from(ds_right) as u64)?;
        vmwrite(vmcs::guest::GS_ACCESS_RIGHTS, u32::from(ds_right) as u64)?;
        vmwrite(vmcs::guest::TR_ACCESS_RIGHTS, u32::from(tr_right) as u64)?;
        vmwrite(
            vmcs::guest::LDTR_ACCESS_RIGHTS,
            u32::from(ldtr_right) as u64,
        )?;

        vmwrite(
            vmcs::guest::CS_SELECTOR,
            x86::segmentation::cs().bits() as u64,
        )?;
        vmwrite(vmcs::guest::SS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::DS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::ES_SELECTOR, 0)?;
        vmwrite(vmcs::guest::FS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::GS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::TR_SELECTOR, 0)?;
        vmwrite(vmcs::guest::LDTR_SELECTOR, 0)?;
        vmwrite(vmcs::guest::FS_BASE, 0)?;
        vmwrite(vmcs::guest::GS_BASE, 0)?;

        vmwrite(vmcs::guest::IA32_EFER_FULL, read_msr(x86::msr::IA32_EFER))?;
        vmwrite(vmcs::guest::RFLAGS, 0x2)?;
        vmwrite(vmcs::guest::LINK_PTR_FULL, u64::MAX)?;

        vmwrite(vmcs::guest::RIP, 0)?; // TODO: Set linux kernel base
                                       // TODO: RSI

        //vmwrite(vmcs::control::CR0_READ_SHADOW, vmread(vmcs::guest::CR0)?)?;
        //vmwrite(vmcs::control::CR4_READ_SHADOW, vmread(vmcs::guest::CR4)?)?;

        info!("Guest State Check (Extended):");
        info!("  CR0: {:#x}", vmread(vmcs::guest::CR0)?);
        info!("  CR3: {:#x}", vmread(vmcs::guest::CR3)?);
        info!("  CR4: {:#x}", vmread(vmcs::guest::CR4)?);
        info!("  EFER: {:#x}", vmread(vmcs::guest::IA32_EFER_FULL)?);
        info!(
            "  CS: sel={:#x}, base={:#x}, limit={:#x}, ar={:#x}",
            vmread(vmcs::guest::CS_SELECTOR)?,
            vmread(vmcs::guest::CS_BASE)?,
            vmread(vmcs::guest::CS_LIMIT)?,
            vmread(vmcs::guest::CS_ACCESS_RIGHTS)?
        );
        info!(
            "  TR: sel={:#x}, base={:#x}, limit={:#x}, ar={:#x}",
            vmread(vmcs::guest::TR_SELECTOR)?,
            vmread(vmcs::guest::TR_BASE)?,
            vmread(vmcs::guest::TR_LIMIT)?,
            vmread(vmcs::guest::TR_ACCESS_RIGHTS)?
        );

        Ok(())
    }
}

impl VCpu for IntelVCpu {
    fn run(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        if !self.activated {
            self.activate(frame_allocator)?;
            self.activated = true;
        }

        self.vmentry().map_err(|e| e.to_str())?;
        self.vmexit_handler()?;

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

        let ept = ept::EPT::new(frame_allocator)?;
        let eptp = ept::EPTP::init(&ept.root_table);

        Ok(IntelVCpu {
            launch_done: false,
            guest_registers: GuestRegisters::default(),
            activated: false,
            vmxon,
            vmcs,
            ept,
            eptp,
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
