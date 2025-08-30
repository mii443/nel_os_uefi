#![allow(non_snake_case)]

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
    pub intercept_all_invlpgb: bool,
    pub intercept_illegally_specified_invlpgb: bool,
    pub intercept_invpcid: bool,
    pub intercept_mcommit: bool,
    pub intercept_tlbsync: bool,
    pub intercept_bus_lock: bool,
    pub intercept_idle_hlt: bool,
    #[skip]
    __: B25,
    // 018h-03Bh
    #[skip]
    __: B128,
    #[skip]
    __: B128,
    #[skip]
    __: B32,
    // 03Ch
    pub pause_filter_threshold: B16,
    // 03Eh
    pub pause_filter_count: B16,
    // 040h
    pub iopm_base_physical_address: B64,
    // 048h
    pub msrpm_base_physical_address: B64,
    // 050h
    pub tsc_offset: B64,
    // 058h
    pub guest_asid: B32,
    pub tlb_control: TlbControl,
    pub allow_larger_rap: bool,
    pub clear_rap_on_vmrun: bool,
    #[skip]
    __: B22,
    // 060h
    pub v_tpr: B8,
    pub v_irq: bool,
    pub vgif: bool,
    pub v_nmi: bool,
    pub v_nmi_mask: bool,
    #[skip]
    __: B3,
    pub v_intr_prio: B4,
    pub v_ign_tpr: bool,
    #[skip]
    __: B3,
    pub v_intr_masking: bool,
    pub amd_virtual_gif: bool,
    pub v_nmi_enable: bool,
    #[skip]
    __: B3,
    pub x2avic_enable: bool,
    pub avic_enable: bool,
    pub v_intr_vector: B8,
    #[skip]
    __: B24,
    // 068h
    pub interrupt_shadow: bool,
    pub guest_interrupt_mask: bool,
    #[skip]
    __: B62,
    // 070h
    pub exit_code: B64,
    // 078h
    pub exit_info1: B64,
    // 080h
    pub exit_info2: B64,
    // 088h
    pub exit_int_info: B64,
    // 090h
    pub np_enable: bool,
    pub enable_sev: bool,
    pub enable_encrypted_state: bool,
    pub guest_mode_execution_trap: bool,
    pub sss_check_enable: bool,
    pub virtual_transparent_encryption: bool,
    pub enable_read_only_guest_page_table: bool,
    pub enable_invlpgb_and_tlbsync: bool,
    #[skip]
    __: B56,
    // 098h
    pub avic_apic_bar: B52,
    #[skip]
    __: B12,
    // 0A0h
    pub ghcb_gpa: B64,
    // 0A8h
    pub event_injection: B64,
    // 0B0h
    pub n_cr3: B64,
    // 0D8h
    pub lbr_virtualization_enable: bool,
    pub vmload_vmsave_virtualization_enable: bool,
    pub ibs_virtualization_enable: bool,
    pub pmc_virtualization_enable: bool,
    #[skip]
    __: B60,
    // 0C0h
    pub vmcb_clean_bits: B32,
    #[skip]
    __: B32,
    // 0C8h
    pub next_rip: B64,
    // 0D0h
    pub fetched_bytes: B8,
    pub gutest_instruction_bytes: B120,
    // 0E0h
    pub avic_apic_backing_page_pointer: B52,
    #[skip]
    __: B12,
    // 0E8-0EFh Reserved
    #[skip]
    __: B64,
    #[skip]
    __: B64,
    #[skip]
    __: B64,
    #[skip]
    __: B64,
    // 0F0h
    #[skip]
    __: B12,
    pub avic_logical_table_pointer: B40,
    #[skip]
    __: B12,
    // 0F8h
    pub avic_physical_max_index: B12,
    pub avic_physical_table_pointer: B40,
    #[skip]
    __: B12,
    // 100h-107h Reserved
    #[skip]
    __: B64,
    // 108h
    #[skip]
    __: B12,
    pub vmsa_pointer: B40,
    #[skip]
    __: B12,
    // 110h
    pub vmgexit_rax: B64,
    // 118h
    pub vmgexit_cpl: B8,
}

#[derive(Specifier, Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TlbControl {
    DoNothing = 0,
    FlushAll = 1,
    _RESERVED1 = 2,
    FlushGuest = 3,
    _RESERVED2 = 4,
    FlushHost = 5,
    _RESERVED3 = 6,
    FlushGuestNonGlobal = 7,
}
