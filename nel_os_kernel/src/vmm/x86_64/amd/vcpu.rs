use raw_cpuid::cpuid;
use x86_64::{
    instructions::interrupts,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::{
    error, info,
    vmm::{
        VCpu,
        x86_64::{
            amd::{
                npt::Npt,
                register::GuestRegisters,
                vmcb::{Flags1, InterceptVector1, InterceptVector2, Vmcb, VmcbSegment},
            },
            common::{self, X86VCpu, segment::*, write_msr},
        },
    },
};

pub struct AMDVCpu {
    initialized: bool,
    exit_reported: bool,
    vmcb: Vmcb,
    hsave: PhysFrame,
    npt: Npt,
    guest_registers: GuestRegisters,
    guest_memory_size: u64,
}

const GUEST_MEMORY_SIZE: u64 = 256 * 1024 * 1024;
const PAGE_SIZE: u64 = 4096;

impl AMDVCpu {
    pub fn setup(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str>
    where
        Self: X86VCpu,
    {
        info!("Setting up AMD VCPU for a Linux guest");

        self.init_guest_memory(frame_allocator)?;
        common::linux::load_kernel(self)?;
        self.setup_linux_segments();

        {
            let raw_vmcb = self.vmcb.get_raw_vmcb();
            raw_vmcb
                .control_area
                .intercept_vec1
                .set(InterceptVector1::SHUTDOWN, true);
            raw_vmcb
                .control_area
                .intercept_vec1
                .set(InterceptVector1::CPUID, true);

            raw_vmcb
                .control_area
                .intercept_vec2
                .set(InterceptVector2::VMRUN, true);

            raw_vmcb.control_area.guest_asid = 1;
            raw_vmcb.control_area.flags1.set(Flags1::NP_ENABLE, true);
            raw_vmcb.control_area.nested_page_table_cr3 =
                self.npt.root_table.start_address().as_u64();

            // Linux's 32-bit boot entry starts with paging and long mode off.
            // SVME remains set because it is required by SVM guest-state
            // validation; nested VMRUN is intercepted above.
            raw_vmcb.state_save_area.efer = 1 << 12;
            raw_vmcb.state_save_area.rip = common::linux::LAYOUT_KERNEL_BASE;
            raw_vmcb.state_save_area.rsp = 0;
            info!("Guest RIP set to {:x}", raw_vmcb.state_save_area.rip);

            // PE | ET | NE, with paging disabled as required by the Linux
            // protected-mode boot protocol.
            raw_vmcb.state_save_area.cr0 = (1 << 0) | (1 << 4) | (1 << 5);
            raw_vmcb.state_save_area.cr3 = 0;
            raw_vmcb.state_save_area.cr4 = 0;
            raw_vmcb.state_save_area.dr6 = 0xffff_0ff0;
            raw_vmcb.state_save_area.dr7 = 0x400;
            raw_vmcb.state_save_area.rflags = 0x2;
            raw_vmcb.state_save_area.rax = 0;
            raw_vmcb.state_save_area.g_pat = 0x0007_0406_0007_0406;
            raw_vmcb.control_area.vmcb_clean_bits = 0;
            raw_vmcb.control_area.tlb_control = 1;
            raw_vmcb.state_save_area.cpl = 0;
        }

        self.guest_registers.rsi = common::linux::LAYOUT_BOOTPARAM;

        self.dump_guest_state();

        Ok(())
    }

    fn setup_linux_segments(&mut self) {
        let raw_vmcb = self.vmcb.get_raw_vmcb();

        let code_attrib = common::segment::SegmentRights {
            accessed: true,
            rw: true,
            dc: false,
            executable: true,
            desc_type: common::segment::DescriptorType::Code,
            dpl: 0,
            present: true,
            avl: false,
            long: false,
            db: true,
            granularity: common::segment::Granularity::KByte,
        }
        .to_amd_segment_attrib();
        let data_attrib = common::segment::SegmentRights {
            accessed: true,
            rw: true,
            dc: false,
            executable: false,
            desc_type: common::segment::DescriptorType::Code,
            dpl: 0,
            present: true,
            avl: false,
            long: false,
            db: true,
            granularity: common::segment::Granularity::KByte,
        }
        .to_amd_segment_attrib();
        let tr_attrib = common::segment::SegmentRights {
            accessed: true,
            rw: true,
            dc: false,
            executable: true,
            desc_type: common::segment::DescriptorType::System,
            dpl: 0,
            present: true,
            avl: false,
            long: false,
            db: false,
            granularity: common::segment::Granularity::Byte,
        }
        .to_amd_segment_attrib();

        raw_vmcb.state_save_area.cs.selector = 0;
        raw_vmcb.state_save_area.cs.attrib = code_attrib;
        raw_vmcb.state_save_area.cs.limit = u32::MAX;
        raw_vmcb.state_save_area.cs.base = 0;

        for segment in [
            &mut raw_vmcb.state_save_area.ss,
            &mut raw_vmcb.state_save_area.ds,
            &mut raw_vmcb.state_save_area.es,
            &mut raw_vmcb.state_save_area.fs,
            &mut raw_vmcb.state_save_area.gs,
        ] {
            segment.selector = 0;
            segment.attrib = data_attrib;
            segment.limit = u32::MAX;
            segment.base = 0;
        }

        raw_vmcb.state_save_area.tr.selector = 0;
        raw_vmcb.state_save_area.tr.attrib = tr_attrib;
        raw_vmcb.state_save_area.tr.base = 0;
        raw_vmcb.state_save_area.tr.limit = 0;

        raw_vmcb.state_save_area.gdtr = VmcbSegment {
            selector: 0,
            attrib: 0,
            limit: 0,
            base: 0,
        };
        raw_vmcb.state_save_area.idtr = raw_vmcb.state_save_area.gdtr;
        raw_vmcb.state_save_area.ldtr = VmcbSegment {
            selector: 0,
            attrib: 0x82,
            limit: 0,
            base: 0,
        };
    }

    fn init_guest_memory(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        info!(
            "Allocating {} MiB of AMD guest RAM",
            self.guest_memory_size / 1024 / 1024
        );
        let mut gpa = 0;
        while gpa < self.guest_memory_size {
            let frame = frame_allocator
                .allocate_frame()
                .ok_or("No free frames for guest RAM")?;
            unsafe {
                core::ptr::write_bytes(
                    frame.start_address().as_u64() as *mut u8,
                    0,
                    PAGE_SIZE as usize,
                );
            }
            self.npt
                .map_4k(gpa, frame.start_address().as_u64(), frame_allocator)?;
            gpa += PAGE_SIZE;
        }

        // Allow a nested Linux guest to access the legacy IOAPIC and LAPIC
        // MMIO pages exposed by the outer QEMU machine.
        for mmio_gpa in [0xfec0_0000, 0xfee0_0000] {
            self.npt.map_4k(mmio_gpa, mmio_gpa, frame_allocator)?;
        }
        Ok(())
    }

    fn handle_cpuid(&mut self) {
        let leaf = self.vmcb.get_raw_vmcb().state_save_area.rax as u32;
        let subleaf = self.guest_registers.rcx as u32;
        let mut result = core::arch::x86_64::__cpuid_count(leaf, subleaf);

        match leaf {
            // L1 does not yet implement KVM paravirtual MSRs for L2. Hide the
            // hypervisor bit so Linux selects its native AMD clock path.
            0x0000_0001 => result.ecx &= !(1 << 31),
            // Do not expose another level of SVM until its state is fully
            // virtualized by this VMM.
            0x8000_0001 => result.ecx &= !(1 << 2),
            0x4000_0000..=0x4000_00ff | 0x8000_000a => {
                result.eax = 0;
                result.ebx = 0;
                result.ecx = 0;
                result.edx = 0;
            }
            _ => {}
        }

        let vmcb = self.vmcb.get_raw_vmcb();
        vmcb.state_save_area.rax = result.eax as u64;
        self.guest_registers.rbx = result.ebx as u64;
        self.guest_registers.rcx = result.ecx as u64;
        self.guest_registers.rdx = result.edx as u64;
        vmcb.state_save_area.rip = vmcb.control_area.next_rip;
        vmcb.control_area.vmcb_clean_bits = 0;
    }

    fn dump_guest_state(&mut self) {
        let raw_vmcb = self.vmcb.get_raw_vmcb();
        let cs_selector = raw_vmcb.state_save_area.cs.selector;
        let cs_attrib = raw_vmcb.state_save_area.cs.attrib;
        let cs_base = raw_vmcb.state_save_area.cs.base;
        let cs_limit = raw_vmcb.state_save_area.cs.limit;
        let ss_selector = raw_vmcb.state_save_area.ss.selector;
        let ss_attrib = raw_vmcb.state_save_area.ss.attrib;
        let ss_base = raw_vmcb.state_save_area.ss.base;
        let ss_limit = raw_vmcb.state_save_area.ss.limit;
        let tr_selector = raw_vmcb.state_save_area.tr.selector;
        let tr_attrib = raw_vmcb.state_save_area.tr.attrib;
        let tr_base = raw_vmcb.state_save_area.tr.base;
        let tr_limit = raw_vmcb.state_save_area.tr.limit;
        let gdtr_base = raw_vmcb.state_save_area.gdtr.base;
        let gdtr_limit = raw_vmcb.state_save_area.gdtr.limit;
        let idtr_base = raw_vmcb.state_save_area.idtr.base;
        let idtr_limit = raw_vmcb.state_save_area.idtr.limit;
        info!(
            "AMD guest state: cr0={:#x} cr3={:#x} cr4={:#x} efer={:#x} rflags={:#x}",
            raw_vmcb.state_save_area.cr0,
            raw_vmcb.state_save_area.cr3,
            raw_vmcb.state_save_area.cr4,
            raw_vmcb.state_save_area.efer,
            raw_vmcb.state_save_area.rflags
        );
        info!(
            "AMD guest RIP={:#x} RSP={:#x} CPL={}",
            raw_vmcb.state_save_area.rip,
            raw_vmcb.state_save_area.rsp,
            raw_vmcb.state_save_area.cpl
        );
        info!(
            "AMD guest CS sel={:#x} attr={:#x} base={:#x} limit={:#x}",
            cs_selector, cs_attrib, cs_base, cs_limit
        );
        info!(
            "AMD guest SS sel={:#x} attr={:#x} base={:#x} limit={:#x}",
            ss_selector, ss_attrib, ss_base, ss_limit
        );
        info!(
            "AMD guest TR sel={:#x} attr={:#x} base={:#x} limit={:#x}",
            tr_selector, tr_attrib, tr_base, tr_limit
        );
        info!(
            "AMD guest GDTR base={:#x} limit={:#x} IDTR base={:#x} limit={:#x}",
            gdtr_base, gdtr_limit, idtr_base, idtr_limit
        );
    }

    fn get_segment(&mut self, segment: Segment) -> &mut VmcbSegment {
        let raw_vmcb = self.vmcb.get_raw_vmcb();

        match segment {
            Segment::ES => &mut raw_vmcb.state_save_area.es,
            Segment::CS => &mut raw_vmcb.state_save_area.cs,
            Segment::SS => &mut raw_vmcb.state_save_area.ss,
            Segment::DS => &mut raw_vmcb.state_save_area.ds,
            Segment::FS => &mut raw_vmcb.state_save_area.fs,
            Segment::GS => &mut raw_vmcb.state_save_area.gs,
            Segment::GDTR => &mut raw_vmcb.state_save_area.gdtr,
            Segment::LDTR => &mut raw_vmcb.state_save_area.ldtr,
            Segment::IDTR => &mut raw_vmcb.state_save_area.idtr,
            Segment::TR => &mut raw_vmcb.state_save_area.tr,
        }
    }
}

impl VCpu for AMDVCpu {
    fn run(
        &mut self,
        frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        interrupts::without_interrupts(|| unsafe {
            if !self.initialized {
                self.setup(frame_allocator)?;
                self.initialized = true;
            }

            {
                let vmcb = self.vmcb.get_raw_vmcb();
                vmcb.control_area.exit_code = 0;
                vmcb.control_area.exit_info1 = 0;
                vmcb.control_area.exit_info2 = 0;
                vmcb.control_area.tlb_control = 0;
            }

            write_msr(0xC001_0117, self.hsave.start_address().as_u64());

            super::asm::asm_vmrun(
                self.vmcb.frame.start_address().as_u64(),
                &mut self.guest_registers,
            );

            let vmcb = self.vmcb.get_raw_vmcb();
            let exit_code = vmcb.control_area.exit_code;
            if !self.exit_reported || !matches!(exit_code, 0x72 | 0x78) {
                info!(
                    "VMEXIT: code={:#x} info1={:#x} info2={:#x} next_rip={:#x}",
                    exit_code,
                    vmcb.control_area.exit_info1,
                    vmcb.control_area.exit_info2,
                    vmcb.control_area.next_rip
                );
                self.exit_reported = true;
            }

            match exit_code as u32 {
                0x72 => {
                    self.handle_cpuid();
                    Ok(())
                }
                0x78 => Ok(()), // HLT
                0x7f => Err("AMD guest shutdown (likely a triple fault)"),
                0x400 => Err("AMD nested page fault"),
                u32::MAX => Err("VMRUN rejected the VMCB guest state"),
                _ => Err("Unhandled AMD VMEXIT"),
            }
        })
    }

    fn write_memory(&mut self, addr: u64, data: u8) -> Result<(), &'static str> {
        self.npt.set(addr, data)
    }

    fn write_memory_ranged(
        &mut self,
        addr_start: u64,
        addr_end: u64,
        data: u8,
    ) -> Result<(), &'static str> {
        self.npt.set_range(addr_start, addr_end, data)
    }

    fn read_memory(&mut self, addr: u64) -> Result<u8, &'static str> {
        self.npt.get(addr)
    }

    fn write_memory_slice(&mut self, addr: u64, data: &[u8]) -> Result<(), &'static str> {
        self.npt.set_slice(addr, data)
    }

    fn get_guest_memory_size(&self) -> u64 {
        self.guest_memory_size
    }

    fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str>
    where
        Self: Sized,
    {
        let mut efer = common::read_msr(0xc000_0080);
        efer |= 1 << 12;
        common::write_msr(0xc000_0080, efer);

        let hsave = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate frame for VCPU HSave area")?;
        unsafe {
            core::ptr::write_bytes(hsave.start_address().as_u64() as *mut u8, 0, 4096);
        }

        Ok(AMDVCpu {
            initialized: false,
            exit_reported: false,
            vmcb: Vmcb::new(frame_allocator)?,
            hsave,
            npt: Npt::new(frame_allocator)?,
            guest_registers: GuestRegisters::default(),
            guest_memory_size: GUEST_MEMORY_SIZE,
        })
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

impl X86VCpu for AMDVCpu {
    fn set_segment_rights(
        &mut self,
        segment: common::segment::Segment,
        rights: common::segment::SegmentRights,
    ) {
        let seg = self.get_segment(segment);
        seg.attrib = rights.to_amd_segment_attrib();
    }

    fn set_segment_base(&mut self, segment: common::segment::Segment, base: u64) {
        let seg = self.get_segment(segment);
        seg.base = base;
    }

    fn set_segment_limit(&mut self, segment: common::segment::Segment, limit: u32) {
        let seg = self.get_segment(segment);
        seg.limit = limit;
    }

    fn set_segment_selector(&mut self, segment: common::segment::Segment, selector: u16) {
        let seg = self.get_segment(segment);
        seg.selector = selector;
    }
}
