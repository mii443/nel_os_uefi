use modular_bitfield::{bitfield, prelude::*};
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

#[bitfield]
pub struct RawVmcb {
    // 000h
    pub intercept_cr_read: B16,
    pub intercept_cr_write: B16,
    // 004h
    pub intercept_dr_read: B16,
    pub intercept_dr_write: B16,
    // 008h
    pub intercept_exceptions: B32,
    // 00Ch
    pub intercept_intr: bool,
    pub intercept_nmi: bool,
    pub intercept_smi: bool,
    pub intercept_init: bool,
    pub intercept_vintr: bool,
    pub intercept_cr0_write_ts_or_mp: bool,
    pub intercept_read_idtr: bool,
    pub intercept_read_gdtr: bool,
    pub intercept_read_ldtr: bool,
    pub intercept_read_tr: bool,
    pub intercept_write_idtr: bool,
    pub intercept_write_gdtr: bool,
    pub intercept_write_ldtr: bool,
    pub intercept_write_tr: bool,
    pub intercept_rdtsc: bool,
    pub intercept_rdpmc: bool,
    pub intercept_pushf: bool,
    pub intercept_popf: bool,
    pub intercept_cpuid: bool,
    pub intercept_rsm: bool,
    pub intercept_iret: bool,
    pub intercept_int_n: bool,
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
    pub intercept_vmcall: bool,
    pub intercept_vmload: bool,
    pub intercept_vmsave: bool,
    pub intercept_stgi: bool,
    pub intercept_clgi: bool,
    pub intercept_skinit: bool,
    pub intercept_rdtscp: bool,
    pub intercept_icebp: bool,
    pub intercept_wbinvd_and_wbnoinvd: bool,
    pub intercept_monitor_and_monitorx: bool,
    pub intercept_mwait_and_mwaitx_unconditionally: bool,
    pub intercept_mwait_and_mwaitx: bool,
    pub intercept_xsetbv: bool,
    pub intercept_rdpru: bool,
    pub intercept_write_efer_after_guest_inst_finish: bool,
    pub intercept_write_cr0_after_guest_inst_finish: B16,
    // 014h
}
