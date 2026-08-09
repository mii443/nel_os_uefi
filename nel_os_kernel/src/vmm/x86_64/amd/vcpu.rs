use core::arch::asm;

use raw_cpuid::cpuid;
use x86::{
    controlregs::{cr0, cr3, cr4},
    dtables::{self, DescriptorTablePointer},
    segmentation::{cs, ds},
    task,
};
use x86_64::{
    instructions::interrupts,
    registers::debug::{Dr6, Dr7},
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::{
    error, info,
    vmm::{
        VCpu,
        x86_64::{
            amd::vmcb::{InterceptVector1, InterceptVector2, Vmcb, VmcbSegment},
            common::{self, X86VCpu, read_msr, segment::*, write_msr},
        },
    },
};

pub struct AMDVCpu {
    initialized: bool,
    exit_reported: bool,
    vmcb: Vmcb,
    hsave: PhysFrame,
}

const GUEST_STACK_SIZE: usize = 4096 * 4;

#[repr(align(16))]
struct GuestStack([u8; GUEST_STACK_SIZE]);

static mut GUEST_STACK: GuestStack = GuestStack([0; GUEST_STACK_SIZE]);

impl AMDVCpu {
    #[unsafe(no_mangle)]
    extern "C" fn guest_fn() {
        unsafe {
            loop {
                asm!("hlt");
            }
        }
    }

    pub fn setup(&mut self) -> Result<(), &'static str>
    where
        Self: X86VCpu,
    {
        info!("Setting up AMD VCPU");

        self.setup_segments_from_host();

        {
            let raw_vmcb = self.vmcb.get_raw_vmcb();
            raw_vmcb
                .control_area
                .intercept_vec1
                .set(InterceptVector1::HLT, true);

            raw_vmcb
                .control_area
                .intercept_vec2
                .set(InterceptVector2::VMRUN, true);

            raw_vmcb.control_area.guest_asid = 1;

            // SVME must remain set in the guest EFER for VMRUN state
            // validation. Nested VMRUN is still prevented by its mandatory
            // intercept below.
            raw_vmcb.state_save_area.efer = read_msr(x86::msr::IA32_EFER);
            raw_vmcb.state_save_area.rip = AMDVCpu::guest_fn as *const () as u64;
            let stack_base = unsafe { (&raw mut GUEST_STACK.0) as *mut u8 as u64 };
            // A SysV64 function enters with RSP == 8 (mod 16), after the
            // caller's return address would normally have been pushed.
            raw_vmcb.state_save_area.rsp = stack_base + GUEST_STACK_SIZE as u64 - 8;
            info!("Guest RIP set to {:x}", raw_vmcb.state_save_area.rip);

            raw_vmcb.state_save_area.cr0 = unsafe { cr0() }.bits() as u64;
            raw_vmcb.state_save_area.cr3 = unsafe { cr3() };
            raw_vmcb.state_save_area.cr4 = unsafe { cr4() }.bits() as u64;
            raw_vmcb.state_save_area.dr6 = Dr6::read_raw();
            raw_vmcb.state_save_area.dr7 = Dr7::read_raw();
            raw_vmcb.state_save_area.rflags = 0x2;
            raw_vmcb.state_save_area.rax = 0;
            raw_vmcb.control_area.vmcb_clean_bits = 0;
            raw_vmcb.control_area.tlb_control = 0;
            raw_vmcb.state_save_area.cpl = (raw_vmcb.state_save_area.cs.selector & 0b11) as u8;

            raw_vmcb.state_save_area.ldtr.selector = 0;
            raw_vmcb.state_save_area.ldtr.attrib = 0;
            raw_vmcb.state_save_area.ldtr.limit = 0;
            raw_vmcb.state_save_area.ldtr.base = 0;
        }

        self.dump_guest_state();

        Ok(())
    }

    fn setup_segments_from_host(&mut self) {
        let cs_selector = cs().bits() as u16;
        let data_selector = ds().bits() as u16;
        let tr_selector = unsafe { task::tr() }.bits();

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
            long: true,
            db: false,
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

        raw_vmcb.state_save_area.cs.selector = cs_selector;
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
            segment.selector = data_selector;
            segment.attrib = data_attrib;
            segment.limit = u32::MAX;
            segment.base = 0;
        }

        raw_vmcb.state_save_area.fs.base = read_msr(x86::msr::IA32_FS_BASE);
        raw_vmcb.state_save_area.gs.base = read_msr(x86::msr::IA32_GS_BASE);

        raw_vmcb.state_save_area.tr.selector = tr_selector;
        raw_vmcb.state_save_area.tr.attrib = tr_attrib;

        let mut gdtp = DescriptorTablePointer::<u64>::default();
        let mut idtp = DescriptorTablePointer::<u64>::default();
        unsafe {
            dtables::sgdt(&mut gdtp);
            dtables::sidt(&mut idtp);
        }
        raw_vmcb.state_save_area.gdtr.base = gdtp.base as u64;
        raw_vmcb.state_save_area.gdtr.limit = gdtp.limit as u32;
        raw_vmcb.state_save_area.idtr.base = idtp.base as u64;
        raw_vmcb.state_save_area.idtr.limit = idtp.limit as u32;

        let tr_index = (tr_selector as usize) >> 3;
        let gdt_base = gdtp.base as *const u64;
        let tr_low = unsafe { *gdt_base.add(tr_index) };
        let tr_high = unsafe { *gdt_base.add(tr_index + 1) };
        raw_vmcb.state_save_area.tr.base = ((tr_low >> 16) & 0x00ff_ffff)
            | ((tr_low >> 32) & 0xff00_0000)
            | ((tr_high & 0xffff_ffff) << 32);
        raw_vmcb.state_save_area.tr.limit =
            ((tr_low & 0xffff) | (((tr_low >> 48) & 0xf) << 16)) as u32;
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
        _frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        interrupts::without_interrupts(|| unsafe {
            if !self.initialized {
                self.setup().expect("Failed to setup AMD VCPU");
                self.initialized = true;
            }

            let vmcb = self.vmcb.get_raw_vmcb();

            vmcb.control_area.exit_code = 0;
            vmcb.control_area.exit_info1 = 0;
            vmcb.control_area.exit_info2 = 0;

            write_msr(0xC001_0117, self.hsave.start_address().as_u64());

            super::vmrun(self.vmcb.frame.start_address().as_u64());

            let exit_code = vmcb.control_area.exit_code;
            if !self.exit_reported || exit_code != 0x78 {
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
                0x78 => Ok(()), // HLT
                u32::MAX => Err("VMRUN rejected the VMCB guest state"),
                _ => Err("Unhandled AMD VMEXIT"),
            }
        })
    }

    fn write_memory(&mut self, _addr: u64, _data: u8) -> Result<(), &'static str> {
        unimplemented!("AMDVCpu::write_memory is not implemented yet");
    }

    fn write_memory_ranged(
        &mut self,
        _addr_start: u64,
        _addr_end: u64,
        _data: u8,
    ) -> Result<(), &'static str> {
        unimplemented!("AMDVCpu::write_memory_ranged is not implemented yet");
    }

    fn read_memory(&mut self, _addr: u64) -> Result<u8, &'static str> {
        unimplemented!("AMDVCpu::read_memory is not implemented yet");
    }

    fn get_guest_memory_size(&self) -> u64 {
        unimplemented!("AMDVCpu::get_guest_memory_size is not implemented yet")
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
