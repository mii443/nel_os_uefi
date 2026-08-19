use core::{
    arch::{asm, x86_64::_xgetbv},
    sync::atomic::{AtomicU16, Ordering},
};

use raw_cpuid::cpuid;
use spin::Once;
use x86_64::{
    VirtAddr,
    registers::control::{Cr4, Cr4Flags},
    structures::paging::{FrameAllocator, Size4KiB},
};

use crate::{
    constant::PAGE_SIZE,
    info, interrupt,
    network::{PassthroughDescriptor, PassthroughNic},
    storage::{GuestVirtioBlock, VirtioBlock},
    vmm::{
        VCpu,
        x86_64::{
            common::{self, X86VCpu, fxsave::FxState, read_msr, xsave::HostXsaveState},
            intel::{
                auditor, controls, cpuid, debug, ept,
                fpu::{self, XCR0},
                io::{IOBitmap, vmm_interrupt_subscriber},
                msr::{self, ShadowMsr},
                qual::{QualCr, QualIo},
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
    },
};
const TEMP_STACK_SIZE: usize = 4096;
static mut TEMP_STACK: [u8; TEMP_STACK_SIZE + 0x10] = [0; TEMP_STACK_SIZE + 0x10];
static VMXON_REGION: Once<Result<vmxon::Vmxon, &'static str>> = Once::new();

#[repr(C)]
pub struct IntelVCpu {
    pub launch_done: bool,
    pub guest_registers: GuestRegisters,
    // CR2 is not part of VMCS guest/host state.  Preserve it explicitly so
    // that a page fault interrupted by a time-slice VMEXIT cannot observe a
    // different VM's fault address after scheduling resumes.
    pub host_cr2: u64,
    pub guest_cr2: u64,
    pub(super) guest_cr8: u8,
    pub(super) guest_debug_registers: [u64; 8],
    pub host_fx_state: FxState,
    pub guest_fx_state: FxState,
    pub host_xsave_addr: u64,
    pub host_xsave_mask: u64,
    host_xsave_state: HostXsaveState,
    activated: bool,
    guest_memory_initialized: bool,
    guest_memory_initialization_failed: bool,
    interrupt_subscribed: bool,
    halted: bool,
    /// Grants one immediate scheduler retry after a HLT exit leaves an IRQ.
    /// The retry is consumed before injection is attempted, so a masked or
    /// otherwise undeliverable IRQ cannot spin for the whole time slice.
    halted_irq_retry: bool,
    host_pending_irq: AtomicU16,
    vmcs: vmcs::Vmcs,
    ept: ept::Ept,
    eptp: ept::Eptp,
    guest_memory_size: u64,
    guest_memory_allocated: u64,
    pub host_msr: ShadowMsr,
    pub guest_msr: ShadowMsr,
    pub ia32e_enabled: bool,
    pub pic: super::io::Pic,
    io_bitmap: IOBitmap,
    preemption_timer_ticks: u32,
    passthrough: Option<PassthroughNic>,
    guest_block: GuestVirtioBlock,
    fw_cfg: common::fw_cfg::FwCfg,
    pm_timer: u32,
    pub host_xcr0: u64,
    pub guest_xcr0: XCR0,
}

impl IntelVCpu {
    #[unsafe(no_mangle)]
    unsafe extern "C" fn intel_set_host_stack(rsp: u64) {
        vmwrite(x86::vmx::vmcs::host::RSP, rsp).unwrap();
    }

    fn vmexit_handler(&mut self, block: Option<&mut VirtioBlock>) -> Result<(), &'static str> {
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
            return Err("VMEntry failure");
        } else {
            let basic_reason = (exit_reason_raw & 0xFFFF) as u16;
            let exit_reason: VmxExitReason = basic_reason
                .try_into()
                .map_err(|_| "Unknown VMX exit reason")?;
            let interrupted_event = self.preserve_interrupted_event()?;

            match exit_reason {
                VmxExitReason::HLT => {
                    // VMX reports the intercepted HLT at the instruction RIP.
                    // Retire it once, then keep the VCPU out of VM entry until
                    // its own emulated device has a deliverable interrupt.
                    self.step_next_inst()?;
                    vmwrite(vmcs::guest::ACTIVITY_STATE, 0)?;
                    vmwrite(vmcs::guest::INTERRUPTIBILITY_STATE, 0)?;
                    self.halted = true;
                }
                VmxExitReason::CPUID => {
                    cpuid::handle_cpuid_vmexit(self);
                    self.step_next_inst()?;
                }
                VmxExitReason::RDMSR => {
                    if msr::ShadowMsr::handle_read_msr_vmexit(self).is_ok() {
                        self.step_next_inst()?;
                    } else {
                        self.pic.inject_exception(13, Some(0))?;
                    }
                }
                VmxExitReason::WRMSR => {
                    if msr::ShadowMsr::handle_wrmsr_vmexit(self).is_ok() {
                        self.step_next_inst()?;
                    } else {
                        self.pic.inject_exception(13, Some(0))?;
                    }
                }
                VmxExitReason::CONTROL_REGISTER_ACCESSES => {
                    let qual = vmread(vmcs::ro::EXIT_QUALIFICATION)?;
                    let qual = QualCr::from(qual);

                    if super::cr::handle_cr_access(self, &qual).is_ok() {
                        self.step_next_inst()?;
                    } else {
                        self.pic.inject_exception(13, Some(0))?;
                    }
                }
                VmxExitReason::MOV_DR => {
                    let qualification = vmread(vmcs::ro::EXIT_QUALIFICATION)?;
                    match debug::handle_debug_register_access(self, qualification)? {
                        debug::DebugAccessOutcome::Completed => self.step_next_inst()?,
                        debug::DebugAccessOutcome::GeneralProtection => {
                            self.pic.inject_exception(13, Some(0))?
                        }
                        debug::DebugAccessOutcome::InvalidOpcode => {
                            self.pic.inject_exception(6, None)?
                        }
                        debug::DebugAccessOutcome::DebugException => {
                            self.pic.inject_exception(1, None)?
                        }
                    }
                }
                VmxExitReason::XSETBV => {
                    let guest_cr4 = vmread(vmcs::guest::CR4)?;
                    let value = ((self.guest_registers.rdx & 0xffff_ffff) << 32)
                        | (self.guest_registers.rax & 0xffff_ffff);
                    if guest_cr4 & Cr4Flags::OSXSAVE.bits() == 0 {
                        self.pic.inject_exception(6, None)?;
                    } else if fpu::set_xcr(self, self.guest_registers.rcx as u32, value).is_ok() {
                        self.step_next_inst()?;
                    } else {
                        self.pic.inject_exception(13, Some(0))?;
                    }
                }
                VmxExitReason::IO_INSTRUCTION => {
                    let qual = vmread(vmcs::ro::EXIT_QUALIFICATION)?;
                    let qual_io = QualIo::from(qual);
                    let size = match qual_io.size() {
                        0 => 1,
                        1 => 2,
                        3 => 4,
                        _ => 0,
                    };
                    let is_config_address = (0x0cf8..=0x0cfb).contains(&qual_io.port());
                    let guest_block_handled = size != 0
                        && !is_config_address
                        && self.guest_block.handles_port(qual_io.port());
                    let passthrough_handled = size != 0
                        && !is_config_address
                        && !guest_block_handled
                        && self
                            .passthrough
                            .as_ref()
                            .is_some_and(|device| device.handles_port(qual_io.port()));
                    let io_result = if size == 0 {
                        Err("Invalid I/O operand size")
                    } else if qual_io.string() != 0 || qual_io.rep() != 0 {
                        if qual_io.direction() != 1 || qual_io.port() != 0x511 || size != 1 {
                            Err("String/REP guest I/O is unsupported")
                        } else {
                            let count = if qual_io.rep() != 0 {
                                self.guest_registers.rcx
                            } else {
                                1
                            };
                            if count > self.guest_memory_size {
                                Err("Intel guest fw_cfg transfer is too large")
                            } else {
                                let es_base = vmread(vmcs::guest::ES_BASE)?;
                                let decrement = vmread(vmcs::guest::RFLAGS)? & (1 << 10) != 0;
                                for _ in 0..count {
                                    let address = es_base.wrapping_add(self.guest_registers.rdi);
                                    let value = self.fw_cfg.read_u8(self.guest_memory_size);
                                    self.ept.set(address, value)?;
                                    self.guest_registers.rdi = if decrement {
                                        self.guest_registers.rdi.wrapping_sub(1)
                                    } else {
                                        self.guest_registers.rdi.wrapping_add(1)
                                    };
                                }
                                if qual_io.rep() != 0 {
                                    self.guest_registers.rcx = 0;
                                }
                                Ok(())
                            }
                        }
                    } else if is_config_address {
                        if qual_io.direction() == 1 {
                            let value = u64::from(self.guest_block.io_in(qual_io.port(), size, 0));
                            let mask = match size {
                                1 => u8::MAX as u64,
                                2 => u16::MAX as u64,
                                _ => u64::MAX,
                            };
                            self.guest_registers.rax = (self.guest_registers.rax & !mask) | value;
                        } else {
                            let value = self.guest_registers.rax as u32;
                            self.guest_block
                                .write_config_address(qual_io.port(), size, value);
                            if let Some(device) = self.passthrough.as_mut() {
                                device.io_out(qual_io.port(), size, value);
                            }
                        }
                        Ok(())
                    } else if guest_block_handled {
                        let Some(block) = block else {
                            return Err("VM virtio-blk backend is unavailable");
                        };
                        if qual_io.direction() == 1 {
                            let value = u64::from(self.guest_block.io_in(
                                qual_io.port(),
                                size,
                                block.capacity_sectors(),
                            ));
                            let mask = match size {
                                1 => u8::MAX as u64,
                                2 => u16::MAX as u64,
                                _ => u64::MAX,
                            };
                            self.guest_registers.rax = (self.guest_registers.rax & !mask) | value;
                            Ok(())
                        } else {
                            self.guest_block.io_out(
                                qual_io.port(),
                                size,
                                self.guest_registers.rax as u32,
                                &mut self.ept,
                                block,
                            )
                        }
                    } else if passthrough_handled {
                        let device = self.passthrough.as_mut().unwrap();
                        if qual_io.direction() == 1 {
                            let value = u64::from(device.io_in(qual_io.port(), size));
                            let mask = match size {
                                1 => u8::MAX as u64,
                                2 => u16::MAX as u64,
                                _ => u64::MAX,
                            };
                            self.guest_registers.rax = (self.guest_registers.rax & !mask) | value;
                        } else {
                            device.io_out(qual_io.port(), size, self.guest_registers.rax as u32);
                        }
                        Ok(())
                    } else if qual_io.direction() == 1 && qual_io.port() == 0x511 && size == 1 {
                        self.guest_registers.rax = (self.guest_registers.rax & !0xff)
                            | u64::from(self.fw_cfg.read_u8(self.guest_memory_size));
                        Ok(())
                    } else if qual_io.direction() == 0 && qual_io.port() == 0x510 && size == 2 {
                        self.fw_cfg.select(self.guest_registers.rax as u16);
                        Ok(())
                    } else if qual_io.direction() == 1
                        && matches!(qual_io.port(), 0x608 | 0xb008)
                        && size == 4
                    {
                        self.pm_timer = self.pm_timer.wrapping_add(357_954);
                        self.guest_registers.rax = (self.guest_registers.rax & !(u32::MAX as u64))
                            | u64::from(self.pm_timer);
                        Ok(())
                    } else {
                        self.pic.handle_io(&mut self.guest_registers, qual_io)
                    };

                    if io_result.is_ok() {
                        self.step_next_inst()?;
                    } else {
                        self.pic.inject_exception(6, None)?;
                    }
                }
                VmxExitReason::EXTERNAL_INTERRUPT => unsafe {
                    asm!("sti");
                    asm!("nop");
                    asm!("cli");
                },
                VmxExitReason::EPT_VIOLATION => {
                    let guest_address = vmread(vmcs::ro::GUEST_PHYSICAL_ADDR_FULL)?;
                    info!("Ept Violation at guest address: {:#x}", guest_address);
                    return Err("Ept Violation");
                }
                VmxExitReason::TRIPLE_FAULT => {
                    info!("Triple fault detected");
                    return Err("Triple fault");
                }
                VmxExitReason::VMX_PREEMPTION_TIMER_EXPIRED => {
                    // This exit is asynchronous: guest RIP already points at
                    // the next instruction to execute. Returning from run()
                    // gives the outer scheduler a chance to switch VCPUs.
                }
                VmxExitReason::EXCEPTION => {
                    if interrupted_event {
                        return Err("VMX exception collided with interrupted event delivery");
                    }
                    let vmexit_intr_info = vmread(vmcs::ro::VMEXIT_INTERRUPTION_INFO)?;
                    let vector = (vmexit_intr_info & 0xFF) as u32;
                    let has_error_code = (vmexit_intr_info & (1 << 11)) != 0;

                    let error_code = if has_error_code {
                        Some(vmread(vmcs::ro::VMEXIT_INTERRUPTION_ERR_CODE)? as u32)
                    } else {
                        None
                    };

                    let rip = vmread(vmcs::guest::RIP)?;

                    let mut instruction_bytes = [0u8; 16];
                    let mut valid_bytes = 0;

                    match self.translate_guest_address(rip) {
                        Ok(guest_phys_addr) => {
                            for i in 0..16 {
                                match self.ept.get(guest_phys_addr + i) {
                                    Ok(byte) => {
                                        instruction_bytes[i as usize] = byte;
                                        valid_bytes = i + 1;
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                        Err(e) => {
                            info!(
                                "Failed to get physical address for RIP: {:#x}, {:?}",
                                rip, e
                            );
                            return Err("Failed to get physical address for RIP");
                        }
                    }

                    if valid_bytes > 2 && instruction_bytes[..3] == [0x0f, 0x01, 0xca] {
                        let rflags = vmread(vmcs::guest::RFLAGS)?;
                        vmwrite(vmcs::guest::RFLAGS, rflags & !(1 << 18))?;
                        self.step_next_inst()?;
                    } else if valid_bytes > 2 && instruction_bytes[..3] == [0x0f, 0x01, 0xcb] {
                        let rflags = vmread(vmcs::guest::RFLAGS)?;
                        vmwrite(vmcs::guest::RFLAGS, rflags | (1 << 18))?;
                        self.step_next_inst()?;
                    } else {
                        self.pic.inject_exception(vector, error_code)?;
                    }
                }
                _ => {
                    return Err("Unhandled VM exit reason");
                }
            }
        }

        Ok(())
    }

    fn preserve_interrupted_event(&mut self) -> Result<bool, &'static str> {
        use x86::vmx::vmcs;

        let vectoring = vmread(vmcs::ro::IDT_VECTORING_INFO)?;
        if vectoring & (1 << 31) == 0 {
            return Ok(false);
        }

        let entry_info = vmread(vmcs::control::VMENTRY_INTERRUPTION_INFO_FIELD)?;
        let reinjection = vectoring & 0x8000_0fff;
        if entry_info & (1 << 31) != 0 && entry_info != reinjection {
            return Err("VMX event delivery collided with a pending entry event");
        }
        vmwrite(vmcs::control::VMENTRY_INTERRUPTION_INFO_FIELD, reinjection)?;

        if vectoring & (1 << 11) != 0 {
            vmwrite(
                vmcs::control::VMENTRY_EXCEPTION_ERR_CODE,
                vmread(vmcs::ro::IDT_VECTORING_ERR_CODE)?,
            )?;
        }

        let event_type = (vectoring >> 8) & 7;
        if matches!(event_type, 4..=6) {
            vmwrite(
                vmcs::control::VMENTRY_INSTRUCTION_LEN,
                vmread(vmcs::ro::VMEXIT_INSTRUCTION_LEN)?,
            )?;
        }

        Ok(true)
    }

    fn load_guest_xcr0(&mut self) -> Result<(), &'static str> {
        unsafe {
            self.host_xsave_state
                .save_host_and_load_guest(u64::from(self.guest_xcr0))
        }
    }

    fn load_host_xcr0(&mut self) -> Result<(), &'static str> {
        if self.host_xsave_state.is_enabled()
            && unsafe { _xgetbv(0) } != self.host_xsave_state.mask()
        {
            return Err("VM-exit failed to restore host XCR0");
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
        auditor::controls::check_vmcs_control_fields().unwrap();

        let success = {
            let result: u16;

            self.load_guest_xcr0().unwrap();
            unsafe {
                result = crate::vmm::x86_64::intel::asm::asm_vm_entry(self as *mut _);
            };
            self.load_host_xcr0().unwrap();

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
        let timer_shift = controls::setup_exec_controls()?;
        let tsc_khz = interrupt::apic::GUEST_TSC_KHZ
            .get()
            .copied()
            .ok_or("TSC frequency unavailable for VMX preemption timer")?;
        self.preemption_timer_ticks = controls::preemption_timer_ticks(
            tsc_khz,
            crate::vmm::VCPU_TIME_SLICE_MILLIS,
            timer_shift,
        );
        controls::setup_entry_controls()?;
        controls::setup_exit_controls()?;
        Self::setup_host_state()?;
        self.setup_guest_state()?;
        self.io_bitmap.setup()?;

        if !self.interrupt_subscribed {
            x86_64::instructions::interrupts::without_interrupts(|| {
                interrupt::subscriber::subscribe(
                    vmm_interrupt_subscriber,
                    &self.host_pending_irq as *const AtomicU16 as *mut core::ffi::c_void,
                )
            })?;
            self.interrupt_subscribed = true;
        }

        if !self.guest_memory_initialized {
            if self.guest_memory_initialization_failed {
                return Err("Guest memory initialization previously failed");
            }
            if let Err(error) = self.init_guest_memory(frame_allocator) {
                self.guest_memory_initialization_failed = true;
                return Err(error);
            }
            self.guest_memory_initialized = true;
        }

        self.ept
            .set_slice(common::uefi::FIRMWARE_BASE, common::uefi::firmware_image()?)?;

        msr::register_msrs(self).map_err(|_| "MSR error")?;
        msr::_update_msrs(self).map_err(|_| "MSR error")?;

        let cr4 = Cr4::read() | Cr4Flags::OSFXSR;
        unsafe {
            Cr4::write(cr4);
        }

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
            self.guest_memory_allocated =
                self.guest_memory_allocated.saturating_add(PAGE_SIZE as u64);

            unsafe {
                core::ptr::write_bytes(hpa as *mut u8, 0, PAGE_SIZE);
            }
            self.ept.map_4k(gpa, hpa, frame_allocator)?;
            if let Some(device) = self.passthrough.as_mut() {
                device.map_dma(gpa, hpa, frame_allocator)?;
            }
            gpa += 0x1000;
            pages -= 1;
        }

        let firmware = common::uefi::firmware_image()?;
        for (index, page) in firmware.chunks(PAGE_SIZE).enumerate() {
            let frame = frame_allocator
                .allocate_frame()
                .ok_or("No free frames for guest UEFI firmware")?;
            let hpa = frame.start_address().as_u64();
            unsafe {
                core::ptr::write_bytes(hpa as *mut u8, 0, PAGE_SIZE);
                core::ptr::copy_nonoverlapping(page.as_ptr(), hpa as *mut u8, page.len());
            }
            self.ept.map_4k(
                common::uefi::FIRMWARE_BASE + index as u64 * PAGE_SIZE as u64,
                hpa,
                frame_allocator,
            )?;
        }
        for gpa in common::uefi::PLATFORM_MMIO_PAGES {
            let frame = frame_allocator
                .allocate_frame()
                .ok_or("No free frames for guest UEFI platform MMIO")?;
            let hpa = frame.start_address().as_u64();
            unsafe { core::ptr::write_bytes(hpa as *mut u8, 0, PAGE_SIZE) };
            self.ept.map_4k(gpa, hpa, frame_allocator)?;
        }

        if let Some(device) = self.passthrough.as_ref() {
            for (base, size) in device.mmio_regions() {
                let mut address = base;
                while address < base.saturating_add(size) {
                    self.ept.map_4k(address, address, frame_allocator)?;
                    address += PAGE_SIZE as u64;
                }
            }
        }

        let eptp = ept::Eptp::init(&self.ept.root_table);
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
            unsafe { cr4() }.bits() as u64 | Cr4Flags::OSXSAVE.bits(),
        )?;

        vmwrite(
            vmcs::host::RIP,
            crate::vmm::x86_64::intel::asm::asm_vmexit_handler as usize as u64,
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
        vmwrite(
            vmcs::host::TR_BASE,
            interrupt::gdt::loaded_tss_base(tr.bits())?,
        )?;

        vmwrite(vmcs::host::IA32_EFER_FULL, read_msr(x86::msr::IA32_EFER))?;

        Ok(())
    }

    fn setup_guest_state(&mut self) -> Result<(), &'static str> {
        use x86::{controlregs::*, vmx::vmcs};
        let cr0 = Cr0::CR0_EXTENSION_TYPE;
        vmwrite(vmcs::guest::CR0, cr0.bits() as u64)?;
        vmwrite(vmcs::guest::CR3, 0)?;
        // VMCLEAR resets the VMCS launch state but does not clear guest-state
        // fields. Never derive boot CR4 from the previous Linux instance.
        vmwrite(
            vmcs::guest::CR4,
            Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS.bits()
                & !Cr4Flags::PHYSICAL_ADDRESS_EXTENSION.bits(),
        )?;

        vmwrite(vmcs::guest::CS_BASE, common::uefi::RESET_VECTOR_CS_BASE)?;
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
            .with_granularity(Granularity::Byte)
            .with_long(false)
            .with_db(false);

        let ds_right = SegmentRights::default()
            .with_rw(true)
            .with_dc(false)
            .with_executable(false)
            .with_desc_type(DescriptorType::Code)
            .with_dpl(0)
            .with_granularity(Granularity::Byte)
            .with_long(false)
            .with_db(false);

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
            u64::from(common::uefi::RESET_VECTOR_CS),
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

        vmwrite(vmcs::guest::IA32_DEBUGCTL_FULL, 0)?;
        vmwrite(vmcs::guest::IA32_PAT_FULL, 0x0007_0406_0007_0406)?;
        vmwrite(vmcs::guest::IA32_EFER_FULL, 0)?;
        vmwrite(vmcs::guest::IA32_EFER_HIGH, 0)?;
        vmwrite(vmcs::guest::DR7, 0x400)?;
        vmwrite(vmcs::guest::RSP, 0)?;
        vmwrite(vmcs::guest::RFLAGS, 0x2)?;
        vmwrite(vmcs::guest::PENDING_DBG_EXCEPTIONS, 0)?;
        vmwrite(vmcs::guest::INTERRUPTIBILITY_STATE, 0)?;
        vmwrite(vmcs::guest::ACTIVITY_STATE, 0)?;
        vmwrite(vmcs::guest::IA32_SYSENTER_CS, 0)?;
        vmwrite(vmcs::guest::IA32_SYSENTER_ESP, 0)?;
        vmwrite(vmcs::guest::IA32_SYSENTER_EIP, 0)?;
        vmwrite(vmcs::guest::LINK_PTR_FULL, u64::MAX)?;

        vmwrite(vmcs::guest::RIP, common::uefi::RESET_VECTOR_IP)?;
        self.guest_registers = GuestRegisters::default();

        // A pending reinjection belongs to the pre-reset guest and must not
        // leak into the new boot instance.
        vmwrite(vmcs::control::VMENTRY_INTERRUPTION_INFO_FIELD, 0)?;
        vmwrite(vmcs::control::VMENTRY_EXCEPTION_ERR_CODE, 0)?;
        vmwrite(vmcs::control::VMENTRY_INSTRUCTION_LEN, 0)?;

        vmwrite(vmcs::control::CR0_READ_SHADOW, vmread(vmcs::guest::CR0)?)?;
        vmwrite(vmcs::control::CR4_READ_SHADOW, vmread(vmcs::guest::CR4)?)?;

        Ok(())
    }

    fn translate_guest_address(&mut self, vaddr: u64) -> Result<u64, &'static str> {
        let cr3 = vmread(x86::vmx::vmcs::guest::CR3).map_err(|_| "Failed to read guest CR3")?;
        let pml4_base = cr3 & !0xFFF; // Clear lower 12 bits to get page table base

        let efer = vmread(x86::vmx::vmcs::guest::IA32_EFER_FULL).unwrap_or(0);
        let is_long_mode = (efer & (1 << 10)) != 0; // LMA bit

        if !is_long_mode {
            return Ok(vaddr & 0xFFFFFFFF);
        }

        let pml4_idx = (vaddr >> 39) & 0x1FF;
        let pdpt_idx = (vaddr >> 30) & 0x1FF;
        let pd_idx = (vaddr >> 21) & 0x1FF;
        let pt_idx = (vaddr >> 12) & 0x1FF;
        let page_offset = vaddr & 0xFFF;

        let pml4_entry_addr = pml4_base + (pml4_idx * 8);
        let pml4_entry = self.read_guest_phys_u64(pml4_entry_addr)?;
        if (pml4_entry & 1) == 0 {
            return Err("PML4 entry not present");
        }
        let pdpt_base = pml4_entry & 0x000FFFFFFFFFF000;

        let pdpt_entry_addr = pdpt_base + (pdpt_idx * 8);
        let pdpt_entry = self.read_guest_phys_u64(pdpt_entry_addr)?;
        if (pdpt_entry & 1) == 0 {
            return Err("PDPT entry not present");
        }

        if (pdpt_entry & (1 << 7)) != 0 {
            let page_base = pdpt_entry & 0x000FFFFFC0000000;
            return Ok(page_base | (vaddr & 0x3FFFFFFF));
        }
        let pd_base = pdpt_entry & 0x000FFFFFFFFFF000;

        let pd_entry_addr = pd_base + (pd_idx * 8);
        let pd_entry = self.read_guest_phys_u64(pd_entry_addr)?;
        if (pd_entry & 1) == 0 {
            return Err("PD entry not present");
        }

        if (pd_entry & (1 << 7)) != 0 {
            let page_base = pd_entry & 0x000FFFFFFFE00000;
            return Ok(page_base | (vaddr & 0x1FFFFF));
        }
        let pt_base = pd_entry & 0x000FFFFFFFFFF000;

        let pt_entry_addr = pt_base + (pt_idx * 8);
        let pt_entry = self.read_guest_phys_u64(pt_entry_addr)?;
        if (pt_entry & 1) == 0 {
            return Err("PT entry not present");
        }
        let page_base = pt_entry & 0x000FFFFFFFFFF000;

        Ok(page_base | page_offset)
    }

    fn read_guest_phys_u64(&mut self, gpa: u64) -> Result<u64, &'static str> {
        let mut result_bytes = [0u8; 8];

        for i in 0..8 {
            match self.ept.get(gpa + i) {
                Ok(byte) => result_bytes[i as usize] = byte,
                Err(_) => return Err("Failed to read from Ept"),
            }
        }

        Ok(u64::from_le_bytes(result_bytes))
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

        // Ept pointer
        let eptp = vmread(x86::vmx::vmcs::control::EPTP_FULL)?;
        info!("Ept pointer: {:#x}", eptp);

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
    fn prepare(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        if !self.activated {
            self.activate(frame_allocator)?;
            self.dump_vmcs_settings()?;
            self.activated = true;
        }
        Ok(())
    }

    fn run(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
        block: Option<&mut VirtioBlock>,
    ) -> Result<(), &'static str> {
        self.prepare(frame_allocator)?;
        // A different VM may have made its VMCS current since this VM's
        // previous time slice (or since this VM was created).
        self.vmcs.load()?;

        if let Some(device) = self.passthrough.as_mut() {
            let (irq, asserted) = device.poll_interrupt_level();
            if asserted {
                self.pic.pending_irq |= 1 << irq;
            } else {
                self.pic.pending_irq &= !(1 << irq);
            }
        }
        let (irq, asserted) = self.guest_block.interrupt_level();
        if asserted {
            self.pic.pending_irq |= 1 << irq;
        } else {
            self.pic.pending_irq &= !(1 << irq);
        }
        self.pic.pending_irq |= self.host_pending_irq.swap(0, Ordering::AcqRel);
        self.pic.poll_serial_input();
        self.pic.poll_timer();

        // Consume the one retry granted after the previous HLT exit. If the
        // IRQ cannot be injected, the early return below makes is_idle() true
        // rather than polling this halted VCPU until its slice expires.
        self.halted_irq_retry = false;

        let interrupt_injected = self.pic.inject_external_interrupt()?;
        if self.halted {
            if !interrupt_injected {
                return Ok(());
            }
            self.halted = false;
        }

        vmwrite(
            x86::vmx::vmcs::guest::VMX_PREEMPTION_TIMER_VALUE,
            u64::from(self.preemption_timer_ticks.max(1)),
        )?;

        x86_64::instructions::interrupts::without_interrupts(|| self.vmentry())
            .map_err(|e| e.to_str())?;
        self.vmexit_handler(block)?;
        self.halted_irq_retry = self.halted && self.pic.has_pending_interrupt();

        Ok(())
    }

    fn write_memory(&mut self, addr: u64, data: u8) -> Result<(), &'static str> {
        self.ept.set(addr, data)
    }

    fn reset(&mut self) -> Result<(), &'static str> {
        if self.guest_memory_initialization_failed {
            return Err("Guest memory initialization previously failed");
        }
        self.launch_done = false;
        self.activated = false;
        self.guest_registers = GuestRegisters::default();
        self.guest_cr2 = 0;
        self.guest_cr8 = 0;
        self.guest_debug_registers = debug::RESET_DEBUG_REGISTERS;
        self.guest_fx_state = FxState::guest_default();
        self.host_msr.clear();
        self.guest_msr.clear();
        self.ia32e_enabled = false;
        let tsc_khz = interrupt::apic::GUEST_TSC_KHZ
            .get()
            .copied()
            .ok_or("TSC frequency unavailable for Intel VCPU reset")?;
        self.pic = super::io::Pic::new(tsc_khz, self.guest_memory_size);
        self.halted = false;
        self.halted_irq_retry = false;
        self.host_pending_irq.store(0, Ordering::Release);
        if let Some(device) = self.passthrough.as_mut() {
            device.reset();
        }
        self.guest_block.reset();
        self.fw_cfg.reset();
        self.pm_timer = 0;
        self.guest_xcr0 = XCR0::from(1);
        Ok(())
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

    fn get_allocated_guest_memory_size(&self) -> u64 {
        self.guest_memory_allocated
    }

    fn is_idle(&self) -> bool {
        self.halted && !self.halted_irq_retry
    }

    fn new(
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
        _hardware_vcpu_id: usize,
        guest_memory_size: u64,
        passthrough: Option<PassthroughDescriptor>,
    ) -> Result<Self, &'static str>
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

        match VMXON_REGION.call_once(|| {
            let mut vmxon = vmxon::Vmxon::new(frame_allocator)?;
            vmxon.activate()?;
            Ok(vmxon)
        }) {
            Ok(_) => {}
            Err(error) => return Err(*error),
        }

        let vmcs = vmcs::Vmcs::new(frame_allocator)?;

        let ept = ept::Ept::new(frame_allocator)?;
        let eptp = ept::Eptp::init(&ept.root_table);
        let host_xsave_state = HostXsaveState::new(frame_allocator)?;
        let host_xsave_addr = host_xsave_state.addr();
        let host_xsave_mask = host_xsave_state.mask();
        let tsc_khz = interrupt::apic::GUEST_TSC_KHZ
            .get()
            .copied()
            .ok_or("TSC frequency unavailable for Intel VCPU")?;
        let host_msr =
            ShadowMsr::new(frame_allocator).map_err(|_| "Failed to allocate host MSR area")?;
        let guest_msr =
            ShadowMsr::new(frame_allocator).map_err(|_| "Failed to allocate guest MSR area")?;
        let passthrough = passthrough
            .map(|descriptor| PassthroughNic::new(descriptor, frame_allocator))
            .transpose()?;

        Ok(IntelVCpu {
            launch_done: false,
            guest_registers: GuestRegisters::default(),
            host_cr2: 0,
            guest_cr2: 0,
            guest_cr8: 0,
            guest_debug_registers: debug::RESET_DEBUG_REGISTERS,
            host_fx_state: FxState::zeroed(),
            guest_fx_state: FxState::guest_default(),
            host_xsave_addr,
            host_xsave_mask,
            host_xsave_state,
            activated: false,
            guest_memory_initialized: false,
            guest_memory_initialization_failed: false,
            interrupt_subscribed: false,
            halted: false,
            halted_irq_retry: false,
            host_pending_irq: AtomicU16::new(0),
            vmcs,
            ept,
            eptp,
            guest_memory_size,
            guest_memory_allocated: 0,
            host_msr,
            guest_msr,
            ia32e_enabled: false,
            pic: super::io::Pic::new(tsc_khz, guest_memory_size),
            io_bitmap: IOBitmap::new(frame_allocator),
            preemption_timer_ticks: 1,
            passthrough,
            guest_block: GuestVirtioBlock::new(),
            fw_cfg: common::fw_cfg::FwCfg::new(),
            pm_timer: 0,
            host_xcr0: host_xsave_mask,
            guest_xcr0: XCR0::from(1),
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

impl X86VCpu for IntelVCpu {
    fn set_segment_rights(
        &mut self,
        segment: common::segment::Segment,
        rights: common::segment::SegmentRights,
    ) {
        todo!()
    }

    fn set_segment_base(&mut self, segment: common::segment::Segment, base: u64) {
        todo!()
    }

    fn set_segment_limit(&mut self, segment: common::segment::Segment, limit: u32) {
        todo!()
    }

    fn set_segment_selector(&mut self, segment: common::segment::Segment, selector: u16) {
        todo!()
    }
}
