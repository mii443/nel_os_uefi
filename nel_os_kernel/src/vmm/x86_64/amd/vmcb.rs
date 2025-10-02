#![allow(non_snake_case)]

use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

pub struct Vmcb {
    pub frame: PhysFrame,
}

impl Vmcb {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate VMCB frame")?;
        Ok(Vmcb { frame })
    }

    pub fn get_raw_vmcb(&self) -> &mut RawVmcb {
        let ptr = self.frame.start_address().as_u64() as *mut RawVmcb;
        unsafe { &mut *ptr }
    }
}

#[repr(C, align(4096))]
#[derive(Debug, Clone, Copy)]
pub struct RawVmcb {
    pub control_area: VmcbControlArea,
}

#[repr(C, align(1024))]
#[derive(Debug, Clone, Copy)]
pub struct VmcbControlArea {}

#[repr(C, align(1024))]
#[derive(Debug, Clone, Copy)]
pub struct VmcbStateSaveArea {
    pub intercept_cr_read: u16,
    pub intercept_cr_write: u16,
    pub intercept_dr_read: u16,
    pub intercept_dr_write: u16,
    pub intercept_exception: u32,
    pub intercept_intr: bool,
    pub intercept_nmi: bool,
    pub intercept_smi: bool,
    pub intercept_init: bool,
    pub intercept_vintr: bool,
    pub intercept_cr0_writes_o_ts_mp: bool,
    pub intercept_idtr_read: bool,
    pub intercept_gdtr_read: bool,
    pub intercept_ldtr_read: bool,
    pub intercept_tr_read: bool,
    pub intercept_idtr_write: bool,
    pub intercept_gdtr_write: bool,
    pub intercept_ldtr_write: bool,
    pub intercept_tr_write: bool,
    pub intercept_rdtsc: bool,
    pub intercept_rdpmc: bool,
    pub intercept_pushf: bool,
    pub intercept_popf: bool,
    pub intercept_cpuid: bool,
    pub intercept_rsm: bool,
    pub intercept_iret: bool,
    pub intercept_intn: bool,
    pub intercept_invd: bool,
    pub intercept_pause: bool,
    pub intercept_hlt: bool,
    pub intercept_invlpg: bool,
    pub intercept_invlpga: bool,
    pub intercept_ioio_prot: bool,
    pub intercept_msr_prot: bool,
    pub intercept_task_switch: bool,
    pub intercept_ferr_freeze: bool,
    pub intercept_shutdown: bool,
    // 010h
    pub intercept_vmrun: bool,
    pub intercept_vmmcall: bool,
    pub intercept_vmload: bool,
    pub intercept_vmsave: bool,
    pub intercept_stgi: bool,
    pub intercept_clgi: bool,
    pub intercept_skinit: bool,
    pub intercept_rdtscp: bool,
    pub intercept_icebp: bool,
    pub intercept_wbinvd: bool,
    pub intercept_monitor: bool,
    pub intercept_mwait: bool,
    pub intercept_xsetbv: bool,
    pub intercept_rdpru: bool,
    pub intercept_efer_write: bool,
    pub intercept_cr_write_after_inst: bool,
    // 014h
    pub intercept_all_invlpgb: bool,
    pub intercept_only_illegal_invlpgb: bool,
    pub intercept_invpcid: bool,
    pub intercept_mcommit: bool,
    pub intercept_tlbsync: bool,
    pub intercept_bus_lock: bool,
    pub intercept_hlt_not_pending: bool,
}
