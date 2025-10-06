use core::arch::asm;

use raw_cpuid::cpuid;
use x86::controlregs::{cr0, cr3, cr4};
use x86_64::{
    instructions::interrupts,
    structures::paging::{FrameAllocator, Size4KiB},
};

use crate::{
    error, info,
    vmm::{
        x86_64::{
            amd::vmcb::{InterceptVector1, InterceptVector2, Vmcb, VmcbSegment},
            common::{self, read_msr, segment::*, X86VCpu},
        },
        VCpu,
    },
};

pub struct AMDVCpu {
    initialized: bool,
    vmcb: Vmcb,
}

impl AMDVCpu {
    #[unsafe(no_mangle)]
    fn guest_fn() {
        unsafe {
            asm!("hlt");
        }
    }

    pub fn setup(&mut self) -> Result<(), &'static str>
    where
        Self: X86VCpu,
    {
        info!("Setting up AMD VCPU");

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

        raw_vmcb.state_save_area.efer = read_msr(0xc000_0080);

        raw_vmcb.state_save_area.rip = AMDVCpu::guest_fn as u64;
        info!("Guest RIP set to {:x}", raw_vmcb.state_save_area.rip);

        raw_vmcb.state_save_area.cr0 = unsafe { cr0() }.bits() as u64;
        raw_vmcb.state_save_area.cr3 = unsafe { cr3() };
        raw_vmcb.state_save_area.cr4 = unsafe { cr4() }.bits() as u64;

        setup_segments(self);

        Ok(())
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
            info!("VMCB: {:?}", self.vmcb.get_raw_vmcb());
            info!(
                "VMCB Control area Size: {}",
                core::mem::size_of::<crate::vmm::x86_64::amd::vmcb::VmcbControlArea>()
            );
            info!(
                "VMCB State Save area Size: {}",
                core::mem::size_of::<crate::vmm::x86_64::amd::vmcb::VmcbStateSaveArea>()
            );
            info!(
                "VMCB Size: {}",
                core::mem::size_of_val(self.vmcb.get_raw_vmcb())
            );
            info!(
                "VMCB Physical Address: {:x}",
                self.vmcb.frame.start_address().as_u64()
            );
            super::vmrun(self.vmcb.frame.start_address().as_u64());
        });
        Ok(())
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

        Ok(AMDVCpu {
            initialized: false,
            vmcb: Vmcb::new(frame_allocator)?,
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
