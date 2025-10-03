#![allow(non_snake_case)]

use bitflags::bitflags;
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

bitflags! {
    /// Intercept Vector 1 (Offset 0x00C)
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InterceptVector1: u32 {
        const INTR                    = 1 << 0;
        const NMI                     = 1 << 1;
        const SMI                     = 1 << 2;
        const INIT                    = 1 << 3;
        const VINTR                   = 1 << 4;
        const CR0_WRITES_O_TS_MP      = 1 << 5;
        const IDTR_READ               = 1 << 6;
        const GDTR_READ               = 1 << 7;
        const LDTR_READ               = 1 << 8;
        const TR_READ                 = 1 << 9;
        const IDTR_WRITE              = 1 << 10;
        const GDTR_WRITE              = 1 << 11;
        const LDTR_WRITE              = 1 << 12;
        const TR_WRITE                = 1 << 13;
        const RDTSC                   = 1 << 14;
        const RDPMC                   = 1 << 15;
        const PUSHF                   = 1 << 16;
        const POPF                    = 1 << 17;
        const CPUID                   = 1 << 18;
        const RSM                     = 1 << 19;
        const IRET                    = 1 << 20;
        const INTN                    = 1 << 21;
        const INVD                    = 1 << 22;
        const PAUSE                   = 1 << 23;
        const HLT                     = 1 << 24;
        const INVLPG                  = 1 << 25;
        const INVLPGA                 = 1 << 26;
        const IOIO_PROT               = 1 << 27;
        const MSR_PROT                = 1 << 28;
        const TASK_SWITCH             = 1 << 29;
        const FERR_FREEZE             = 1 << 30;
        const SHUTDOWN                = 1 << 31;
    }
}

bitflags! {
    /// Intercept Vector 2 (Offset 0x010)
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InterceptVector2: u32 {
        const VMRUN                   = 1 << 0;
        const VMMCALL                 = 1 << 1;
        const VMLOAD                  = 1 << 2;
        const VMSAVE                  = 1 << 3;
        const STGI                    = 1 << 4;
        const CLGI                    = 1 << 5;
        const SKINIT                  = 1 << 6;
        const RDTSCP                  = 1 << 7;
        const ICEBP                   = 1 << 8;
        const WBINVD                  = 1 << 9;
        const MONITOR                 = 1 << 10;
        const MWAIT                   = 1 << 11;
        const XSETBV                  = 1 << 12;
        const RDPRU                   = 1 << 13;
        const EFER_WRITE              = 1 << 14;
        const CR_WRITE_AFTER_INST     = 1 << 15;
    }
}

bitflags! {
    /// Intercept Vector 3 (Offset 0x014)
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InterceptVector3: u32 {
        const ALL_INVLPGB             = 1 << 0;
        const ONLY_ILLEGAL_INVLPGB    = 1 << 1;
        const INVPCID                 = 1 << 2;
        const MCOMMIT                 = 1 << 3;
        const TLBSYNC                 = 1 << 4;
        const BUS_LOCK                = 1 << 5;
        const HLT_NOT_PENDING         = 1 << 6;
    }
}

bitflags! {
    /// VMCB Clean Bits
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct VmcbCleanBits: u32 {
        const INTERCEPTS              = 1 << 0;
        const IOPM                    = 1 << 1;
        const ASID                    = 1 << 2;
        const TPR                     = 1 << 3;
        const NP                      = 1 << 4;
        const CR                      = 1 << 5;
        const DR                      = 1 << 6;
        const DT                      = 1 << 7;
        const SEG                     = 1 << 8;
        const CR2                     = 1 << 9;
        const LBR                     = 1 << 10;
        const AVIC                    = 1 << 11;
    }
}

bitflags! {
    /// Interrupt Flags 1
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InterruptFlags1: u8 {
        const V_IRQ                   = 1 << 0;
        const VGIF                    = 1 << 1;
        const V_NMI                   = 1 << 2;
        const V_NMI_MASK              = 1 << 3;
    }
}

bitflags! {
    /// Interrupt Flags 2
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InterruptFlags2: u8 {
        const V_INTR_MASKING          = 1 << 0;
        const AMD_V_GIF               = 1 << 1;
        const V_NMI_ENABLE            = 1 << 2;
        const X2AVIC_ENABLE           = 1 << 6;
        const AVIC_ENABLE             = 1 << 7;
    }
}

bitflags! {
    /// Interrupt Shadow Flags
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InterruptShadowFlags: u64 {
        const INTERRUPT_SHADOW        = 1 << 0;
        const GUEST_INTERRUPT_MASK    = 1 << 1;
    }
}

bitflags! {
    /// Flags 1
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Flags1: u64 {
        const NP_ENABLE               = 1 << 0;
        const SEV_ENABLE              = 1 << 1;
        const ENC_SEV_STATE           = 1 << 2;
        const GUEST_MODE_EXEC_TRAP    = 1 << 3;
        const SSS_CHECK_ENABLE        = 1 << 4;
        const V_TRANSPAR_ENCRYPTION   = 1 << 5;
        const RO_GUEST_PAGE_TABLES    = 1 << 6;
        const ENABLE_INVLPGB_TLBSYNC  = 1 << 7;
    }
}

bitflags! {
    /// Virtualization Flags
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct VirtualizationFlags: u64 {
        const LBT_VIRT_ENABLE         = 1 << 0;
        const VMSAVE_LOAD_VIRT_ENABLE = 1 << 1;
        const IBS_VIRT_ENABLE         = 1 << 2;
        const PMC_VIRT_ENABLE         = 1 << 3;
    }
}

#[repr(C, align(1024))]
#[derive(Debug, Clone, Copy)]
pub struct VmcbControlArea {
    // Offset 0x000
    pub intercept_cr_read: u16,
    pub intercept_cr_write: u16,
    pub intercept_dr_read: u16,
    pub intercept_dr_write: u16,
    pub intercept_exception: u32,

    // Offset 0x00C - Intercept Vectors
    pub intercept_vec1: InterceptVector1,
    pub intercept_vec2: InterceptVector2,
    pub intercept_vec3: InterceptVector3,

    // Offset 0x018 - Reserved
    _reserved1: [u8; 0x03c - 0x018],

    // Offset 0x03C
    pub pause_filter_threshold: u16,
    pub pause_filter_count: u16,

    // Offset 0x040
    pub iopm_base_pa: u64,
    pub msrpm_base_pa: u64,
    pub tsc_offset: u64,

    // Offset 0x058
    pub guest_asid: u32,
    pub tlb_control: u8,
    pub rap_flags: u8,
    _reserved2: [u8; 2],

    // Offset 0x060
    pub v_tpr: u8,
    pub interrupt_flags1: InterruptFlags1,
    pub v_intr_prio_v_ign_tpr: u8,
    pub interrupt_flags2: InterruptFlags2,
    pub v_intr_vector: u8,
    _reserved3: [u8; 3],

    // Offset 0x068
    pub interrupt_shadow_flags: InterruptShadowFlags,

    // Offset 0x070
    pub exit_code: u64,

    // Offset 0x078
    pub exit_info1: u64,

    // Offset 0x080
    pub exit_info2: u64,

    // Offset 0x088
    pub exit_int_info: u64,

    // Offset 0x090
    pub flags1: Flags1,

    // Offset 0x098
    pub avic_apic_bar: u64,

    // Offset 0x0A0
    pub ghcb_gpa: u64,

    // Offset 0x0A8
    pub event_injection: u64,

    // Offset 0x0B0
    pub nested_page_table_cr3: u64,

    // Offset 0x0B8
    pub virtualization_flags: VirtualizationFlags,

    // Offset 0x0C0
    pub vmcb_clean_bits: u32,
    _reserved4: u32,

    // Offset 0x0C8
    pub next_rip: u64,

    // Offset 0x0D0
    pub number_of_bytes_fetched: u8,
    pub guest_instruction_bytes: [u8; 15],

    // Offset 0x0E0
    pub avic_apic_backing_page_ptr: u64,

    // Offset 0x0E8-0x0EF - Reserved
    _reserved5: [u8; 0x0F0 - 0x0E8],

    // Offset 0x0F0
    pub avic_logical_table_ptr: u64,

    // Offset 0x0F8
    pub avic_physical_table_ptr_max_index: u64,

    // Offset 0x100-0x107 - Reserved
    _reserved6: [u8; 0x108 - 0x100],

    // Offset 0x108
    pub vmsa_pointer: u64,

    // Offset 0x110
    pub vmgexit_rax: u64,

    // Offset 0x118
    pub vmgexit_cpl: u8,

    // Offset 0x120
    pub bus_lock_threshold_counter: u16,

    // Offset 0x128-0x133 - Reserved
    _reserved7: [u8; 0x134 - 0x128],

    // Offset 0x134
    pub update_irr: bool,
    _reserved8: u8,

    // Offset 0x138
    pub allowed_sev_features_mask: u64,

    // Offset 0x140
    pub guest_sev_features: u64,

    // Offset 0x148-0x149 - Reserved
    _reserved9: [u8; 0x150 - 0x148],

    // Offset 0x150
    pub requested_irr: [u64; 4],

    // 0x170-0x3FF - Reserved
    _reserved10: [u8; 0x400 - 0x170],
}

#[repr(C, align(1024))]
#[derive(Debug, Clone, Copy)]
pub struct VmcbStateSaveArea {}
