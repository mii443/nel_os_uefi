use core::arch::naked_asm;

use raw_cpuid::cpuid;
use x86_64::{
    structures::paging::{FrameAllocator, Size4KiB},
    VirtAddr,
};

use crate::{
    info,
    vmm::{
        x86_64::{
            common::{self, read_msr},
            intel::{
                controls, cpuid, ept,
                msr::{self, ShadowMsr},
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
    guest_memory_size: u64,
    pub host_msr: ShadowMsr,
    pub guest_msr: ShadowMsr,
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
                    self.step_next_inst()?;
                }
                VmxExitReason::CPUID => {
                    info!("VM exit reason: CPUID");
                    cpuid::handle_cpuid_vmexit(self);
                    self.step_next_inst()?;
                }
                VmxExitReason::RDMSR => {
                    msr::ShadowMsr::handle_read_msr_vmexit(self);
                    self.step_next_inst()?;
                }
                VmxExitReason::WRMSR => {
                    msr::ShadowMsr::handle_wrmsr_vmexit(self);
                    self.step_next_inst()?;
                }
                VmxExitReason::EPT_VIOLATION => {
                    let guest_address = vmread(vmcs::ro::GUEST_PHYSICAL_ADDR_FULL)?;
                    info!("EPT Violation at guest address: {:#x}", guest_address);
                    return Err("EPT Violation");
                }
                _ => {
                    info!("VM exit reason: {:?}", exit_reason);
                    return Err("Unhandled VM exit reason");
                }
            }
        }

        Ok(())
    }

    fn step_next_inst(&mut self) -> Result<(), &'static str> {
        use x86::vmx::vmcs;
        let rip = vmread(vmcs::guest::RIP)?;
        vmwrite(
            vmcs::guest::RIP,
            rip + vmread(vmcs::ro::VMEXIT_INSTRUCTION_LEN)?,
        )?;
        Ok(())
    }

    fn vmentry(&mut self) -> Result<(), InstructionError> {
        msr::update_msrs(self).unwrap();

        let success = {
            let result: u16;
            unsafe {
                result = crate::vmm::x86_64::intel::asm::asm_vm_entry(self as *mut _);
            };
            result == 0
        };

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
        self.setup_guest_state()?;
        msr::register_msrs(self).map_err(|_| "MSR error")?;

        self.init_guest_memory(frame_allocator)?;

        common::linux::load_kernel(self)?;

        Ok(())
    }

    fn init_guest_memory(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        let mut pages = self.guest_memory_size / 0x1000;
        let mut gpa = 0;

        while pages > 0 {
            let frame = frame_allocator.allocate_frame().ok_or("No free frames")?;
            let hpa = frame.start_address().as_u64();

            self.ept.map_4k(gpa, hpa, frame_allocator)?;
            gpa += 0x1000;
            pages -= 1;
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

    fn setup_guest_state(&mut self) -> Result<(), &'static str> {
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

        vmwrite(vmcs::guest::CS_LIMIT, u32::MAX as u64)?;
        vmwrite(vmcs::guest::SS_LIMIT, u32::MAX as u64)?;
        vmwrite(vmcs::guest::DS_LIMIT, u32::MAX as u64)?;
        vmwrite(vmcs::guest::ES_LIMIT, u32::MAX as u64)?;
        vmwrite(vmcs::guest::FS_LIMIT, u32::MAX as u64)?;
        vmwrite(vmcs::guest::GS_LIMIT, u32::MAX as u64)?;
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
            .with_long(false)
            .with_db(true);

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

        vmwrite(vmcs::guest::CS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::SS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::DS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::ES_SELECTOR, 0)?;
        vmwrite(vmcs::guest::FS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::GS_SELECTOR, 0)?;
        vmwrite(vmcs::guest::TR_SELECTOR, 0)?;
        vmwrite(vmcs::guest::LDTR_SELECTOR, 0)?;
        vmwrite(vmcs::guest::FS_BASE, 0)?;
        vmwrite(vmcs::guest::GS_BASE, 0)?;

        vmwrite(vmcs::guest::IA32_EFER_FULL, 0)?;
        vmwrite(vmcs::guest::IA32_EFER_HIGH, 0)?;
        vmwrite(vmcs::guest::RFLAGS, 0x2)?;
        vmwrite(vmcs::guest::LINK_PTR_FULL, u64::MAX)?;

        vmwrite(vmcs::guest::RIP, common::linux::LAYOUT_KERNEL_BASE)?;
        self.guest_registers.rsi = common::linux::LAYOUT_BOOTPARAM;

        //vmwrite(vmcs::control::CR0_READ_SHADOW, vmread(vmcs::guest::CR0)?)?;
        //vmwrite(vmcs::control::CR4_READ_SHADOW, vmread(vmcs::guest::CR4)?)?;

        Ok(())
    }

    fn dump_vmcs_settings(&self) -> Result<(), &'static str> {
        info!("=== VMCS Control Fields ===");

        // Pin-based controls
        let pin_ctrl = vmread(x86::vmx::vmcs::control::PINBASED_EXEC_CONTROLS)?;
        info!("Pin-based VM-execution controls: {:#x}", pin_ctrl);

        // Primary processor-based controls
        let primary_ctrl = vmread(x86::vmx::vmcs::control::PRIMARY_PROCBASED_EXEC_CONTROLS)?;
        info!(
            "Primary processor-based VM-execution controls: {:#x}",
            primary_ctrl
        );

        // Secondary processor-based controls
        let secondary_ctrl = vmread(x86::vmx::vmcs::control::SECONDARY_PROCBASED_EXEC_CONTROLS)?;
        info!(
            "Secondary processor-based VM-execution controls: {:#x}",
            secondary_ctrl
        );

        // Entry controls
        let entry_ctrl = vmread(x86::vmx::vmcs::control::VMENTRY_CONTROLS)?;
        info!("VM-entry controls: {:#x}", entry_ctrl);

        // Exit controls
        let exit_ctrl = vmread(x86::vmx::vmcs::control::VMEXIT_CONTROLS)?;
        info!("VM-exit controls: {:#x}", exit_ctrl);

        // EPT pointer
        let eptp = vmread(x86::vmx::vmcs::control::EPTP_FULL)?;
        info!("EPT pointer: {:#x}", eptp);

        info!("=== Guest State ===");

        // Control registers
        info!("Guest CR0: {:#x}", vmread(x86::vmx::vmcs::guest::CR0)?);
        info!("Guest CR3: {:#x}", vmread(x86::vmx::vmcs::guest::CR3)?);
        info!("Guest CR4: {:#x}", vmread(x86::vmx::vmcs::guest::CR4)?);

        // Instruction pointer and stack
        info!("Guest RIP: {:#x}", vmread(x86::vmx::vmcs::guest::RIP)?);
        info!("Guest RSP: {:#x}", vmread(x86::vmx::vmcs::guest::RSP)?);
        info!(
            "Guest RFLAGS: {:#x}",
            vmread(x86::vmx::vmcs::guest::RFLAGS)?
        );

        // Segment registers - CS
        info!(
            "Guest CS selector: {:#x}",
            vmread(x86::vmx::vmcs::guest::CS_SELECTOR)?
        );
        info!(
            "Guest CS base: {:#x}",
            vmread(x86::vmx::vmcs::guest::CS_BASE)?
        );
        info!(
            "Guest CS limit: {:#x}",
            vmread(x86::vmx::vmcs::guest::CS_LIMIT)?
        );
        info!(
            "Guest CS access rights: {:#x}",
            vmread(x86::vmx::vmcs::guest::CS_ACCESS_RIGHTS)?
        );

        // Segment registers - SS
        info!(
            "Guest SS selector: {:#x}",
            vmread(x86::vmx::vmcs::guest::SS_SELECTOR)?
        );
        info!(
            "Guest SS base: {:#x}",
            vmread(x86::vmx::vmcs::guest::SS_BASE)?
        );
        info!(
            "Guest SS limit: {:#x}",
            vmread(x86::vmx::vmcs::guest::SS_LIMIT)?
        );
        info!(
            "Guest SS access rights: {:#x}",
            vmread(x86::vmx::vmcs::guest::SS_ACCESS_RIGHTS)?
        );

        // TR
        info!(
            "Guest TR selector: {:#x}",
            vmread(x86::vmx::vmcs::guest::TR_SELECTOR)?
        );
        info!(
            "Guest TR base: {:#x}",
            vmread(x86::vmx::vmcs::guest::TR_BASE)?
        );
        info!(
            "Guest TR limit: {:#x}",
            vmread(x86::vmx::vmcs::guest::TR_LIMIT)?
        );
        info!(
            "Guest TR access rights: {:#x}",
            vmread(x86::vmx::vmcs::guest::TR_ACCESS_RIGHTS)?
        );

        // LDTR
        info!(
            "Guest LDTR selector: {:#x}",
            vmread(x86::vmx::vmcs::guest::LDTR_SELECTOR)?
        );
        info!(
            "Guest LDTR base: {:#x}",
            vmread(x86::vmx::vmcs::guest::LDTR_BASE)?
        );
        info!(
            "Guest LDTR limit: {:#x}",
            vmread(x86::vmx::vmcs::guest::LDTR_LIMIT)?
        );
        info!(
            "Guest LDTR access rights: {:#x}",
            vmread(x86::vmx::vmcs::guest::LDTR_ACCESS_RIGHTS)?
        );

        // GDTR/IDTR
        info!(
            "Guest GDTR base: {:#x}",
            vmread(x86::vmx::vmcs::guest::GDTR_BASE)?
        );
        info!(
            "Guest GDTR limit: {:#x}",
            vmread(x86::vmx::vmcs::guest::GDTR_LIMIT)?
        );
        info!(
            "Guest IDTR base: {:#x}",
            vmread(x86::vmx::vmcs::guest::IDTR_BASE)?
        );
        info!(
            "Guest IDTR limit: {:#x}",
            vmread(x86::vmx::vmcs::guest::IDTR_LIMIT)?
        );

        // MSRs
        info!(
            "Guest IA32_EFER: {:#x}",
            vmread(x86::vmx::vmcs::guest::IA32_EFER_FULL)?
        );

        // Link pointer
        info!(
            "Guest VMCS link pointer: {:#x}",
            vmread(x86::vmx::vmcs::guest::LINK_PTR_FULL)?
        );

        info!("=== Host State ===");

        // Control registers
        info!("Host CR0: {:#x}", vmread(x86::vmx::vmcs::host::CR0)?);
        info!("Host CR3: {:#x}", vmread(x86::vmx::vmcs::host::CR3)?);
        info!("Host CR4: {:#x}", vmread(x86::vmx::vmcs::host::CR4)?);

        // Instruction pointer and stack
        info!("Host RIP: {:#x}", vmread(x86::vmx::vmcs::host::RIP)?);
        info!("Host RSP: {:#x}", vmread(x86::vmx::vmcs::host::RSP)?);

        // Segment selectors
        info!(
            "Host CS selector: {:#x}",
            vmread(x86::vmx::vmcs::host::CS_SELECTOR)?
        );
        info!(
            "Host SS selector: {:#x}",
            vmread(x86::vmx::vmcs::host::SS_SELECTOR)?
        );
        info!(
            "Host DS selector: {:#x}",
            vmread(x86::vmx::vmcs::host::DS_SELECTOR)?
        );
        info!(
            "Host ES selector: {:#x}",
            vmread(x86::vmx::vmcs::host::ES_SELECTOR)?
        );
        info!(
            "Host FS selector: {:#x}",
            vmread(x86::vmx::vmcs::host::FS_SELECTOR)?
        );
        info!(
            "Host GS selector: {:#x}",
            vmread(x86::vmx::vmcs::host::GS_SELECTOR)?
        );
        info!(
            "Host TR selector: {:#x}",
            vmread(x86::vmx::vmcs::host::TR_SELECTOR)?
        );

        // Base addresses
        info!(
            "Host FS base: {:#x}",
            vmread(x86::vmx::vmcs::host::FS_BASE)?
        );
        info!(
            "Host GS base: {:#x}",
            vmread(x86::vmx::vmcs::host::GS_BASE)?
        );
        info!(
            "Host TR base: {:#x}",
            vmread(x86::vmx::vmcs::host::TR_BASE)?
        );
        info!(
            "Host GDTR base: {:#x}",
            vmread(x86::vmx::vmcs::host::GDTR_BASE)?
        );
        info!(
            "Host IDTR base: {:#x}",
            vmread(x86::vmx::vmcs::host::IDTR_BASE)?
        );

        // MSRs
        info!(
            "Host IA32_EFER: {:#x}",
            vmread(x86::vmx::vmcs::host::IA32_EFER_FULL)?
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
            self.dump_vmcs_settings()?;
            self.activated = true;
        }

        self.vmentry().map_err(|e| e.to_str())?;
        self.vmexit_handler()?;

        Ok(())
    }

    fn write_memory(&mut self, addr: u64, data: u8) -> Result<(), &'static str> {
        self.ept.set(addr, data)
    }

    fn write_memory_ranged(
        &mut self,
        addr_start: u64,
        addr_end: u64,
        data: u8,
    ) -> Result<(), &'static str> {
        self.ept.set_range(addr_start, addr_end, data)
    }

    fn read_memory(&mut self, addr: u64) -> Result<u8, &'static str> {
        self.ept.get(addr)
    }

    fn get_guest_memory_size(&self) -> u64 {
        self.guest_memory_size
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
            guest_memory_size: 1024 * 1024 * 256, // 256 MiB
            host_msr: ShadowMsr::new(),
            guest_msr: ShadowMsr::new(),
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
