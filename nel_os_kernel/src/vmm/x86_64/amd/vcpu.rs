use alloc::boxed::Box;
use raw_cpuid::cpuid;
use x86_64::{
    instructions::interrupts,
    registers::control::{Cr4, Cr4Flags},
    structures::paging::{FrameAllocator, PhysFrame},
};

use crate::{
    error, info,
    memory::bitmap::BitmapMemoryTable,
    network::{PassthroughDescriptor, PassthroughNic},
    serial,
    storage::{GuestVirtioBlock, VirtioBlock},
    time,
    vmm::{
        VCpu,
        x86_64::{
            amd::{
                npt::Npt,
                permissions::PermissionMaps,
                register::GuestRegisters,
                vmcb::{
                    Flags1, InterceptVector1, InterceptVector2, InterruptFlags2,
                    InterruptShadowFlags, Vmcb, VmcbSegment,
                },
            },
            common::{
                self, X86VCpu, fxsave::FxState, segment::*, write_msr, xsave::HostXsaveState,
            },
        },
    },
};

pub struct AMDVCpu {
    initialized: bool,
    guest_memory_initialized: bool,
    guest_memory_initialization_failed: bool,
    exit_reported: bool,
    vmcb: Vmcb,
    hsave: PhysFrame,
    host_vmcb_state: PhysFrame,
    npt: Npt,
    permission_maps: PermissionMaps,
    serial: SerialState,
    rtc: RtcState,
    legacy_timer: LegacyTimer,
    halted: bool,
    tsc_aux: u64,
    apic_base: u64,
    local_apic: common::local_apic::LocalApic,
    io_apic: common::io_apic::IoApic,
    nb_cfg: u64,
    host_patch_level: u64,
    guest_registers: GuestRegisters,
    host_fx_state: FxState,
    guest_fx_state: FxState,
    host_xsave_addr: u64,
    host_xsave_mask: u64,
    host_xsave_state: HostXsaveState,
    guest_memory_size: u64,
    guest_memory_allocated: u64,
    guest_asid: u32,
    passthrough: Option<PassthroughNic>,
    guest_block: GuestVirtioBlock,
    fw_cfg: common::fw_cfg::FwCfg,
    pm_timer: common::timer::AcpiPmTimer,
    pm_registers: common::timer::AcpiPmRegisters,
}

struct SerialState {
    ier: u8,
    lcr: u8,
    mcr: u8,
    scratch: u8,
    divisor_low: u8,
    divisor_high: u8,
    fifo_enabled: bool,
    overrun: bool,
    thre_interrupt_pending: bool,
    rx: [u8; 256],
    rx_head: usize,
    rx_len: usize,
}

impl Default for SerialState {
    fn default() -> Self {
        Self {
            ier: 0,
            lcr: 0,
            mcr: 0,
            scratch: 0,
            divisor_low: 0,
            divisor_high: 0,
            fifo_enabled: false,
            overrun: false,
            thre_interrupt_pending: false,
            rx: [0; 256],
            rx_head: 0,
            rx_len: 0,
        }
    }
}

impl SerialState {
    fn enqueue_rx(&mut self, value: u8) {
        if self.rx_len == self.rx.len() {
            self.overrun = true;
            return;
        }
        let tail = (self.rx_head + self.rx_len) % self.rx.len();
        self.rx[tail] = value;
        self.rx_len += 1;
    }

    fn dequeue_rx(&mut self) -> u8 {
        if self.rx_len == 0 {
            return 0;
        }
        let value = self.rx[self.rx_head];
        self.rx_head = (self.rx_head + 1) % self.rx.len();
        self.rx_len -= 1;
        value
    }

    fn rx_remaining(&self) -> usize {
        self.rx.len() - self.rx_len
    }

    fn clear_rx(&mut self) {
        self.rx_head = 0;
        self.rx_len = 0;
        self.overrun = false;
    }

    fn write_ier(&mut self, value: u8) {
        let old_ier = self.ier;
        self.ier = value & 0x0f;
        if old_ier & 0x02 == 0 && self.ier & 0x02 != 0 {
            self.thre_interrupt_pending = true;
        }
    }

    fn write_fcr(&mut self, value: u8) {
        self.fifo_enabled = value & 1 != 0;
        if value & 0x02 != 0 {
            self.clear_rx();
        }
        if value & 0x04 != 0 && self.ier & 0x02 != 0 {
            self.thre_interrupt_pending = true;
        }
    }

    fn interrupt_identification(&mut self) -> u8 {
        let fifo = if self.fifo_enabled { 0xc0 } else { 0 };
        let reason = if self.overrun && self.ier & 0x04 != 0 {
            0x06
        } else if self.rx_len != 0 && self.ier & 0x01 != 0 {
            0x04
        } else if self.thre_interrupt_pending && self.ier & 0x02 != 0 {
            // Reading IIR acknowledges the edge-like THRE indication. The
            // next THR write completes immediately and raises it again.
            self.thre_interrupt_pending = false;
            0x02
        } else {
            0x01
        };
        fifo | reason
    }

    fn line_status(&mut self) -> u8 {
        let mut status = 0x60; // THR and transmitter are always empty.
        if self.rx_len != 0 {
            status |= 0x01;
        }
        if self.overrun {
            status |= 0x02;
            self.overrun = false;
        }
        status
    }

    fn interrupt_pending(&self) -> bool {
        (self.overrun && self.ier & 0x04 != 0)
            || (self.rx_len != 0 && self.ier & 0x01 != 0)
            || (self.thre_interrupt_pending && self.ier & 0x02 != 0)
    }

    fn interrupt_output_enabled(&self) -> bool {
        self.mcr & 0x08 != 0
    }

    fn modem_status(&self) -> u8 {
        if self.mcr & 0x10 == 0 {
            return 0xb0;
        }
        ((self.mcr & 0x02) << 3)
            | ((self.mcr & 0x01) << 5)
            | ((self.mcr & 0x04) << 4)
            | ((self.mcr & 0x08) << 4)
    }
}

struct RtcState {
    selector: u8,
    registers: [u8; 128],
}

impl RtcState {
    fn new(guest_memory_size: u64) -> Self {
        let mut registers = [0; 128];
        // Fixed, valid BCD timestamp: 2000-01-01 00:00:00 (Saturday).
        registers[0x06] = 0x07;
        registers[0x07] = 0x01;
        registers[0x08] = 0x01;
        registers[0x09] = 0x00;
        registers[0x0a] = 0x26; // UIP clear, 32-KHz divider.
        registers[0x0b] = 0x02; // BCD, 24-hour mode, interrupts disabled.
        registers[0x0c] = 0x00;
        registers[0x0d] = 0x80; // Valid RAM/time (VRT).
        registers[0x32] = 0x20; // Conventional BCD century byte.
        // Conventional PC CMOS memory-size fields used by OVMF's QEMU
        // platform PEI code before it has constructed a UEFI memory map.
        registers[0x15] = 0x80; // 640 KiB, little-endian.
        registers[0x16] = 0x02;
        registers[0x17] = 0x00; // 15 MiB between 1 MiB and 16 MiB.
        registers[0x18] = 0x3c;
        registers[0x30] = registers[0x17];
        registers[0x31] = registers[0x18];
        let above_16m = common::uefi::low_memory_size(guest_memory_size)
            .saturating_sub(16 * 1024 * 1024)
            .div_ceil(64 * 1024)
            .min(u16::MAX as u64) as u16;
        registers[0x34] = above_16m as u8;
        registers[0x35] = (above_16m >> 8) as u8;
        let above_4g = common::uefi::high_memory_cmos_units(guest_memory_size);
        registers[0x5b] = above_4g as u8;
        registers[0x5c] = (above_4g >> 8) as u8;
        registers[0x5d] = (above_4g >> 16) as u8;
        Self {
            selector: 0,
            registers,
        }
    }

    fn select(&mut self, value: u8) {
        self.selector = value;
    }

    fn read_selector(&self) -> u8 {
        self.selector
    }

    fn read_data(&self) -> u8 {
        match self.selector & 0x7f {
            0x0a => self.registers[0x0a] & 0x7f,
            0x0c => 0,
            0x0d => self.registers[0x0d] | 0x80,
            index => self.registers[index as usize],
        }
    }

    fn write_data(&mut self, value: u8) {
        let index = (self.selector & 0x7f) as usize;
        self.registers[index] = match index {
            0x0a => value & 0x7f,
            0x0c => 0,
            0x0d => value | 0x80,
            _ => value,
        };
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PicInitPhase {
    Ready,
    Icw2,
    Icw3,
    Icw4,
}

struct PicState {
    master_mask: u8,
    slave_mask: u8,
    master_base: u8,
    slave_base: u8,
    master_phase: PicInitPhase,
    slave_phase: PicInitPhase,
    master_read_isr: bool,
    slave_read_isr: bool,
    master_irr: u8,
    master_isr: u8,
    slave_irr: u8,
    slave_isr: u8,
}

impl PicState {
    fn new() -> Self {
        Self {
            master_mask: u8::MAX,
            slave_mask: u8::MAX,
            master_base: 0x20,
            slave_base: 0x28,
            master_phase: PicInitPhase::Ready,
            slave_phase: PicInitPhase::Ready,
            master_read_isr: false,
            slave_read_isr: false,
            master_irr: 0,
            master_isr: 0,
            slave_irr: 0,
            slave_isr: 0,
        }
    }

    fn read(&self, port: u16) -> u8 {
        match port {
            0x20 if self.master_read_isr => self.master_isr,
            0x20 => self.master_irr,
            0x21 => self.master_mask,
            0xa0 if self.slave_read_isr => self.slave_isr,
            0xa0 => self.slave_irr,
            0xa1 => self.slave_mask,
            _ => u8::MAX,
        }
    }

    fn write(&mut self, port: u16, value: u8) {
        match port {
            0x20 if value & 0x10 != 0 => {
                self.master_phase = PicInitPhase::Icw2;
                self.master_mask = u8::MAX;
                self.master_irr = 0;
                self.master_isr = 0;
            }
            0xa0 if value & 0x10 != 0 => {
                self.slave_phase = PicInitPhase::Icw2;
                self.slave_mask = u8::MAX;
                self.slave_irr = 0;
                self.slave_isr = 0;
            }
            0x20 if value == 0x0a => self.master_read_isr = false,
            0x20 if value == 0x0b => self.master_read_isr = true,
            0xa0 if value == 0x0a => self.slave_read_isr = false,
            0xa0 if value == 0x0b => self.slave_read_isr = true,
            0x20 if value == 0x20 => {
                if self.master_isr != 0 {
                    self.master_isr &= !(1 << self.master_isr.trailing_zeros());
                }
            }
            0x20 if value & 0xf8 == 0x60 => self.master_isr &= !(1 << (value & 7)),
            0xa0 if value == 0x20 => {
                if self.slave_isr != 0 {
                    self.slave_isr &= !(1 << self.slave_isr.trailing_zeros());
                }
            }
            0xa0 if value & 0xf8 == 0x60 => self.slave_isr &= !(1 << (value & 7)),
            0x21 => match self.master_phase {
                PicInitPhase::Icw2 => {
                    self.master_base = value & 0xf8;
                    self.master_phase = PicInitPhase::Icw3;
                }
                PicInitPhase::Icw3 => self.master_phase = PicInitPhase::Icw4,
                PicInitPhase::Icw4 => self.master_phase = PicInitPhase::Ready,
                PicInitPhase::Ready => self.master_mask = value,
            },
            0xa1 => match self.slave_phase {
                PicInitPhase::Icw2 => {
                    self.slave_base = value & 0xf8;
                    self.slave_phase = PicInitPhase::Icw3;
                }
                PicInitPhase::Icw3 => self.slave_phase = PicInitPhase::Icw4,
                PicInitPhase::Icw4 => self.slave_phase = PicInitPhase::Ready,
                PicInitPhase::Ready => self.slave_mask = value,
            },
            _ => {}
        }
    }

    fn raise_irq(&mut self, irq: u8) {
        if irq < 8 {
            self.master_irr |= 1 << irq;
        } else if irq < 16 {
            self.slave_irr |= 1 << (irq - 8);
            self.master_irr |= 1 << 2;
        }
    }

    fn set_irq_level(&mut self, irq: u8, asserted: bool) {
        if asserted {
            self.raise_irq(irq);
        } else if irq < 8 {
            self.master_irr &= !(1 << irq);
        } else if irq < 16 {
            self.slave_irr &= !(1 << (irq - 8));
            if self.slave_irr == 0 {
                self.master_irr &= !(1 << 2);
            }
        }
    }

    fn next_irq(&self) -> Option<u8> {
        if self.master_phase != PicInitPhase::Ready {
            return None;
        }
        let mut pending = self.master_irr & !self.master_mask;
        if self.master_isr != 0 {
            let in_service = self.master_isr.trailing_zeros() as u8;
            pending &= (1u8 << in_service).wrapping_sub(1);
        }
        if pending == 0 {
            return None;
        }
        let master_irq = pending.trailing_zeros() as u8;
        if master_irq != 2 {
            return Some(master_irq);
        }
        let mut slave_pending = self.slave_irr & !self.slave_mask;
        if self.slave_isr != 0 {
            let in_service = self.slave_isr.trailing_zeros() as u8;
            slave_pending &= (1u8 << in_service).wrapping_sub(1);
        }
        (slave_pending != 0).then(|| 8 + slave_pending.trailing_zeros() as u8)
    }

    fn acknowledge(&mut self, irq: u8) {
        if irq < 8 {
            self.master_irr &= !(1 << irq);
            self.master_isr |= 1 << irq;
        } else {
            let slave_irq = irq - 8;
            self.slave_irr &= !(1 << slave_irq);
            self.slave_isr |= 1 << slave_irq;
            self.master_irr &= !(1 << 2);
            self.master_isr |= 1 << 2;
        }
    }
}

struct PitChannel0 {
    tsc_hz: u64,
    access_mode: u8,
    mode: u8,
    reload_low: u8,
    reload: u32,
    deadline: u64,
    armed: bool,
    write_high: bool,
    read_high: bool,
    read_latch: Option<u16>,
}

impl PitChannel0 {
    const PIT_HZ: u64 = 1_193_182;

    fn new(tsc_khz: u64) -> Self {
        Self {
            tsc_hz: tsc_khz * 1000,
            access_mode: 3,
            mode: 3,
            reload_low: 0,
            reload: 0x1_0000,
            deadline: 0,
            armed: false,
            write_high: false,
            read_high: false,
            read_latch: None,
        }
    }

    fn rdtsc() -> u64 {
        unsafe { core::arch::x86_64::_rdtsc() }
    }

    fn period_cycles(&self) -> u64 {
        ((self.reload as u128 * self.tsc_hz as u128 / Self::PIT_HZ as u128) as u64).max(1)
    }

    fn program(&mut self, value: u16) {
        self.reload = if value == 0 { 0x1_0000 } else { value as u32 };
        self.deadline = Self::rdtsc().wrapping_add(self.period_cycles());
        self.armed = true;
        self.read_latch = None;
        self.read_high = false;
    }

    fn write(&mut self, value: u8) {
        match self.access_mode {
            1 => self.program(value as u16),
            2 => self.program((value as u16) << 8),
            3 if !self.write_high => {
                self.reload_low = value;
                self.write_high = true;
            }
            3 => {
                self.write_high = false;
                self.program(u16::from_le_bytes([self.reload_low, value]));
            }
            _ => {}
        }
    }

    fn write_control(&mut self, value: u8) {
        let access_mode = (value >> 4) & 3;
        if access_mode == 0 {
            self.read_latch = Some(self.current_count());
            self.read_high = false;
            return;
        }
        self.access_mode = access_mode;
        self.mode = (value >> 1) & 7;
        if self.mode >= 6 {
            self.mode -= 4;
        }
        self.write_high = false;
        self.read_high = false;
        self.read_latch = None;
    }

    fn current_count(&self) -> u16 {
        if !self.armed {
            return self.reload as u16;
        }
        let remaining_cycles = self.deadline.saturating_sub(Self::rdtsc());
        let ticks = (remaining_cycles as u128 * Self::PIT_HZ as u128 / self.tsc_hz as u128)
            .min(self.reload as u128) as u32;
        ticks as u16
    }

    fn read(&mut self) -> u8 {
        if self.access_mode == 3 && !self.read_high && self.read_latch.is_none() {
            self.read_latch = Some(self.current_count());
        }
        let count = self.read_latch.unwrap_or_else(|| self.current_count());
        let value = match self.access_mode {
            1 => count as u8,
            2 => (count >> 8) as u8,
            _ if self.read_high => (count >> 8) as u8,
            _ => count as u8,
        };
        if self.access_mode == 3 {
            self.read_high = !self.read_high;
            if !self.read_high {
                self.read_latch = None;
            }
        }
        value
    }

    fn poll(&mut self) -> bool {
        if !self.armed || Self::rdtsc() < self.deadline {
            return false;
        }
        if matches!(self.mode, 2 | 3) {
            let period = self.period_cycles();
            let elapsed = Self::rdtsc().wrapping_sub(self.deadline);
            self.deadline = self
                .deadline
                .wrapping_add((elapsed / period + 1).saturating_mul(period));
        } else {
            self.armed = false;
        }
        true
    }

    fn output_high(&self) -> bool {
        !self.armed || Self::rdtsc() >= self.deadline
    }
}

struct LegacyTimer {
    pic: PicState,
    pit: PitChannel0,
    pit_channel2: PitChannel0,
    speaker_control: u8,
}

impl LegacyTimer {
    fn new(tsc_khz: u64) -> Self {
        Self {
            pic: PicState::new(),
            pit: PitChannel0::new(tsc_khz),
            pit_channel2: PitChannel0::new(tsc_khz),
            speaker_control: 0,
        }
    }

    fn write_pit_control(&mut self, value: u8) {
        match value >> 6 {
            0 => self.pit.write_control(value),
            2 => self.pit_channel2.write_control(value),
            _ => {}
        }
    }

    fn speaker_status(&self) -> u8 {
        let output = if self.speaker_control & 1 != 0 && self.pit_channel2.output_high() {
            1 << 5
        } else {
            0
        };
        self.speaker_control | output
    }

    fn write_speaker_control(&mut self, value: u8) {
        self.speaker_control = value & 0x03;
    }
}

const PAGE_SIZE: u64 = 4096;
const GUEST_MEMORY_CHUNK_SIZE: u64 = 2 * 1024 * 1024;

impl AMDVCpu {
    pub fn setup(&mut self, frame_allocator: &mut BitmapMemoryTable) -> Result<(), &'static str>
    where
        Self: X86VCpu,
    {
        info!("Setting up AMD VCPU for a UEFI guest");

        // Without the hardware pause filter, intercepting every PAUSE turns
        // normal spin loops into a VMEXIT storm. Require the architectural
        // filter before using PAUSE as a cooperative timer polling point.
        const SVM_FEATURE_PAUSE_FILTER: u32 = 1 << 10;
        if cpuid!(0x8000_000a).edx & SVM_FEATURE_PAUSE_FILTER == 0 {
            return Err("SVM pause filter is required for AMD timer preemption");
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
        self.npt
            .set_slice(common::uefi::FIRMWARE_BASE, common::uefi::firmware_image()?)?;
        self.setup_uefi_reset_segments();

        {
            let raw_vmcb = self.vmcb.get_raw_vmcb();
            raw_vmcb.control_area.intercept_vec1.insert(
                InterceptVector1::INTR
                    | InterceptVector1::CPUID
                    | InterceptVector1::RDPMC
                    | InterceptVector1::INVD
                    | InterceptVector1::PAUSE
                    | InterceptVector1::HLT
                    | InterceptVector1::INVLPGA
                    | InterceptVector1::IOIO_PROT
                    | InterceptVector1::MSR_PROT
                    | InterceptVector1::SHUTDOWN,
            );

            raw_vmcb.control_area.intercept_vec2.insert(
                InterceptVector2::VMRUN
                    | InterceptVector2::VMMCALL
                    | InterceptVector2::VMLOAD
                    | InterceptVector2::VMSAVE
                    | InterceptVector2::STGI
                    | InterceptVector2::CLGI
                    | InterceptVector2::SKINIT
                    | InterceptVector2::WBINVD
                    | InterceptVector2::MONITOR
                    | InterceptVector2::MWAIT
                    | InterceptVector2::XSETBV
                    | InterceptVector2::RDPRU,
            );

            raw_vmcb.control_area.iopm_base_pa = self.permission_maps.iopm_base_pa();
            raw_vmcb.control_area.msrpm_base_pa = self.permission_maps.msrpm_base_pa();
            raw_vmcb.control_area.pause_filter_count = 4096;
            raw_vmcb
                .control_area
                .interrupt_flags2
                .insert(InterruptFlags2::V_INTR_MASKING);

            raw_vmcb.control_area.guest_asid = self.guest_asid;
            raw_vmcb.control_area.flags1.set(Flags1::NP_ENABLE, true);
            raw_vmcb.control_area.nested_page_table_cr3 =
                self.npt.root_table.start_address().as_u64();

            // Start at the architectural x86 reset vector. SVME remains set
            // because SVM requires it in the guest EFER.
            raw_vmcb.state_save_area.efer = 1 << 12;
            raw_vmcb.state_save_area.rip = common::uefi::RESET_VECTOR_IP;
            raw_vmcb.state_save_area.rsp = 0;
            info!("Guest RIP set to {:x}", raw_vmcb.state_save_area.rip);

            raw_vmcb.state_save_area.cr0 = 1 << 4;
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

        self.guest_registers = GuestRegisters::default();

        self.dump_guest_state();

        Ok(())
    }

    fn setup_uefi_reset_segments(&mut self) {
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
            db: false,
            granularity: common::segment::Granularity::Byte,
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
            db: false,
            granularity: common::segment::Granularity::Byte,
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

        raw_vmcb.state_save_area.cs.selector = common::uefi::RESET_VECTOR_CS;
        raw_vmcb.state_save_area.cs.attrib = code_attrib;
        raw_vmcb.state_save_area.cs.limit = 0xffff;
        raw_vmcb.state_save_area.cs.base = common::uefi::RESET_VECTOR_CS_BASE;

        for segment in [
            &mut raw_vmcb.state_save_area.ss,
            &mut raw_vmcb.state_save_area.ds,
            &mut raw_vmcb.state_save_area.es,
            &mut raw_vmcb.state_save_area.fs,
            &mut raw_vmcb.state_save_area.gs,
        ] {
            segment.selector = 0;
            segment.attrib = data_attrib;
            segment.limit = 0xffff;
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
        frame_allocator: &mut BitmapMemoryTable,
    ) -> Result<(), &'static str> {
        info!(
            "Allocating {} MiB of AMD guest RAM",
            self.guest_memory_size / 1024 / 1024
        );
        // Passthrough DMA needs every IOVA backed in advance. VMs without a
        // passthrough device demand-allocate zeroed 2 MiB chunks on NPT faults.
        if self.passthrough.is_some() {
            for (range_base, range_size) in common::uefi::ram_ranges(self.guest_memory_size) {
                let range_end = range_base.saturating_add(range_size);
                let mut gpa = range_base;
                while gpa < range_end {
                    self.allocate_guest_memory_chunk(gpa, frame_allocator)?;
                    gpa = gpa.saturating_add(GUEST_MEMORY_CHUNK_SIZE);
                }
            }
        }

        let firmware = common::uefi::firmware_image()?;
        for (index, page) in firmware.chunks(PAGE_SIZE as usize).enumerate() {
            let frame = frame_allocator
                .allocate_frame()
                .ok_or("No free frames for guest UEFI firmware")?;
            let hpa = frame.start_address().as_u64();
            unsafe {
                core::ptr::write_bytes(hpa as *mut u8, 0, PAGE_SIZE as usize);
                core::ptr::copy_nonoverlapping(page.as_ptr(), hpa as *mut u8, page.len());
            }
            self.npt.map_4k(
                common::uefi::FIRMWARE_BASE + index as u64 * PAGE_SIZE,
                hpa,
                frame_allocator,
            )?;
        }
        for gpa in common::uefi::PLATFORM_MMIO_PAGES {
            let frame = frame_allocator
                .allocate_frame()
                .ok_or("No free frames for guest UEFI platform MMIO")?;
            let hpa = frame.start_address().as_u64();
            common::uefi::initialize_platform_mmio_page(gpa, hpa);
            if gpa == 0xfec0_0000 {
                self.io_apic.attach_page(hpa);
            } else if gpa == 0xfee0_0000 {
                self.local_apic.attach_page(hpa);
            }
            self.npt.map_4k(gpa, hpa, frame_allocator)?;
        }
        let hpet_capabilities = (69_841_279u64 << 32) | (1 << 13) | 1;
        self.npt
            .set_slice(0xfed0_0000, &hpet_capabilities.to_le_bytes())?;

        if let Some(device) = self.passthrough.as_ref() {
            for (base, size) in device.mmio_regions() {
                let mut address = base;
                while address < base.saturating_add(size) {
                    self.npt.map_4k(address, address, frame_allocator)?;
                    address += PAGE_SIZE;
                }
            }
        }

        Ok(())
    }

    fn allocate_guest_memory_chunk(
        &mut self,
        guest_address: u64,
        frame_allocator: &mut BitmapMemoryTable,
    ) -> Result<(), &'static str> {
        let (range_base, range_size) =
            common::uefi::ram_range_containing(self.guest_memory_size, guest_address)
                .ok_or("NPT fault is outside guest RAM")?;
        let range_end = range_base.saturating_add(range_size);
        let mut gpa = guest_address & !(GUEST_MEMORY_CHUNK_SIZE - 1);
        gpa = gpa.max(range_base);
        let chunk_end = gpa.saturating_add(GUEST_MEMORY_CHUNK_SIZE).min(range_end);

        if chunk_end - gpa == GUEST_MEMORY_CHUNK_SIZE && self.npt.get_phys_addr(gpa).is_none() {
            if let Some(frame) = frame_allocator.allocate_contiguous_frames(512, 512) {
                let hpa = frame.start_address().as_u64();
                unsafe {
                    core::ptr::write_bytes(hpa as *mut u8, 0, GUEST_MEMORY_CHUNK_SIZE as usize)
                };
                self.npt.map_2m(gpa, hpa, frame_allocator)?;
                if let Some(device) = self.passthrough.as_mut() {
                    for offset in (0..GUEST_MEMORY_CHUNK_SIZE).step_by(PAGE_SIZE as usize) {
                        device.map_dma(gpa + offset, hpa + offset, frame_allocator)?;
                    }
                }
                self.guest_memory_allocated = self
                    .guest_memory_allocated
                    .saturating_add(GUEST_MEMORY_CHUNK_SIZE);
                return Ok(());
            }
        }

        while gpa < chunk_end {
            if self.npt.get_phys_addr(gpa).is_none() {
                let frame = frame_allocator
                    .allocate_frame()
                    .ok_or("No free frames for guest RAM")?;
                let hpa = frame.start_address().as_u64();
                unsafe { core::ptr::write_bytes(hpa as *mut u8, 0, PAGE_SIZE as usize) };
                self.npt.map_4k(gpa, hpa, frame_allocator)?;
                if let Some(device) = self.passthrough.as_mut() {
                    device.map_dma(gpa, hpa, frame_allocator)?;
                }
                self.guest_memory_allocated = self.guest_memory_allocated.saturating_add(PAGE_SIZE);
            }
            gpa += PAGE_SIZE;
        }
        Ok(())
    }

    fn handle_cpuid(&mut self) {
        let leaf = self.vmcb.get_raw_vmcb().state_save_area.rax as u32;
        let subleaf = self.guest_registers.rcx as u32;
        let mut result = core::arch::x86_64::__cpuid_count(leaf, subleaf);

        match leaf {
            0x0000_0001 => {
                result.ecx &= !((1 << 12)
                    // A TSC deadline requires a virtual local APIC timer.
                    | (1 << 24)
                    | (1 << 26)
                    | (1 << 27)
                    | (1 << 28)
                    | (1 << 29));
                // Identify the virtual-machine boundary and expose the local
                // APIC interfaces implemented by the VMM.
                result.ecx |= (1 << 21) | (1 << 31);
                // Machine-check and MTRR state is MSR-backed.  Advertising
                // these while the default-deny MSRPM rejects their MSRs sends
                // Linux down a partially initialized firmware-error path.
                result.edx &= !((1 << 7) | (1 << 12) | (1 << 14) | (1 << 28));
                result.edx |= 1 << 9;

                // The VMM exposes one vCPU. Do not leak the physical initial
                // APIC ID or the host's logical-processor count.
                result.ebx = (result.ebx & 0x0000_ffff) | (1 << 16);
            }
            0x0000_000b | 0x0000_001f | 0x8000_0026 => {
                // Enumerate one thread in one core. A zero shift is valid for
                // a level containing a single logical processor.
                result.eax = 0;
                result.ebx = 1;
                result.ecx = match subleaf {
                    0 => 1 << 8,       // SMT level
                    1 => (2 << 8) | 1, // Core level
                    _ => 0,
                };
                result.edx = 0;
                if subleaf > 1 {
                    result.ebx = 0;
                }
            }
            0x0000_0007 => {
                if subleaf != 0 {
                    result.eax = 0;
                    result.ebx = 0;
                    result.ecx = 0;
                    result.edx = 0;
                }
                // IA32_TSC_ADJUST, AVX2/AVX-512, and speculation-control
                // MSRs are not fully virtualized, so do not promise them.
                result.ebx &= !((1 << 1)
                    | (1 << 5)
                    | (1 << 16)
                    | (1 << 17)
                    | (1 << 21)
                    | (1 << 26)
                    | (1 << 27)
                    | (1 << 28)
                    | (1 << 30)
                    | (1 << 31));
                result.ecx &= !((1 << 1)
                    | (1 << 6)
                    | (1 << 9)
                    | (1 << 10)
                    | (1 << 11)
                    | (1 << 12)
                    | (1 << 14)
                    // RDPID exposes the physical host's TSC_AUX unless the
                    // instruction itself is virtualized.
                    | (1 << 22));
                result.edx &= !((1 << 2)
                    | (1 << 3)
                    | (1 << 8)
                    | (1 << 22)
                    | (1 << 23)
                    | (1 << 24)
                    | (1 << 25)
                    | (1 << 26)
                    | (1 << 27)
                    | (1 << 29)
                    | (1 << 31));
            }
            0x0000_000d => {
                result.eax = 0;
                result.ebx = 0;
                result.ecx = 0;
                result.edx = 0;
            }
            // Do not expose another level of SVM until its state is fully
            // virtualized by this VMM.
            0x8000_0001 => {
                result.ecx &= !((1 << 2) | (1 << 11) | (1 << 16));
                // AMD repeats legacy feature flags in this extended leaf;
                // keep it consistent with leaf 1.
                result.edx &= !((1 << 7)
                    | (1 << 9)
                    | (1 << 12)
                    | (1 << 14)
                    // RDTSCP returns the physical host's TSC_AUX.  The MSR
                    // permission map only shadows guest WRMSR/RDMSR, so do
                    // not advertise the instruction until it is intercepted.
                    | (1 << 27));
            }
            0x8000_0008 => {
                // RDPRU and speculative-execution mitigation controls require
                // dedicated virtualization. Hiding them prevents the guest
                // from relying on MSRs which are intentionally intercepted.
                result.ebx &= !((1 << 4)
                    | (1 << 12)
                    | (1 << 14)
                    | (1 << 15)
                    | (1 << 16)
                    | (1 << 17)
                    | (1 << 18)
                    | (1 << 19)
                    | (1 << 24)
                    | (1 << 25)
                    | (1 << 26)
                    | (1 << 28)
                    | (1 << 29)
                    | (1 << 30)
                    | (1 << 31));

                // NC=0 means one core; a zero APIC-ID core-width lets legacy
                // enumeration derive the same single-core topology.
                result.ecx &= !0xffff;
            }
            0x8000_001e => {
                // Extended APIC ID, compute-unit ID/thread count, and node ID
                // all describe CPU 0 as the sole thread/core/node.
                result.eax = 0;
                result.ebx = 0;
                result.ecx = 0;
                result.edx = 0;
            }
            common::cpuid::HYPERVISOR_BASE_LEAF => {
                let (eax, ebx, ecx, edx) = common::cpuid::hypervisor_vendor_leaf();
                result.eax = eax;
                result.ebx = ebx;
                result.ecx = ecx;
                result.edx = edx;
            }
            common::cpuid::KVM_COMPAT_BASE_LEAF => {
                let (eax, ebx, ecx, edx) = common::cpuid::kvm_compat_vendor_leaf();
                result.eax = eax;
                result.ebx = ebx;
                result.ecx = ecx;
                result.edx = edx;
            }
            leaf if leaf == common::cpuid::KVM_COMPAT_BASE_LEAF + 1 => {
                result.eax = 0;
                result.ebx = 0;
                result.ecx = 0;
                result.edx = 0;
            }
            0x4000_0001..=0x4000_00ff | 0x8000_000a => {
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

    fn advance_guest_rip(&mut self) -> Result<(), &'static str> {
        let vmcb = self.vmcb.get_raw_vmcb();
        if vmcb.control_area.next_rip <= vmcb.state_save_area.rip {
            return Err("AMD VMEXIT did not provide a valid next RIP");
        }
        vmcb.state_save_area.rip = vmcb.control_area.next_rip;
        vmcb.control_area.vmcb_clean_bits = 0;
        Ok(())
    }

    fn poll_host_serial(&mut self) {
        // Loopback disconnects the external receiver. Leave host bytes in the
        // outer UART until the guest completes 8250 auto-configuration.
        if self.serial.mcr & 0x10 == 0 {
            let mut input = [0; 16];
            let capacity = self.serial.rx_remaining().min(input.len());
            let received = serial::poll_guest_input(&mut input[..capacity]);
            for &value in &input[..received] {
                self.serial.enqueue_rx(value);
            }
        }
        self.sync_uart_irq();
    }

    fn sync_uart_irq(&mut self) {
        let asserted = self.serial.interrupt_output_enabled() && self.serial.interrupt_pending();
        self.legacy_timer.pic.set_irq_level(4, asserted);
        self.io_apic.set_irq_level(4, asserted);
    }

    fn prepare_device_interrupt(&mut self) -> Result<(), &'static str> {
        if self.local_apic.take_mmio_eoi() == Some(true) {
            self.io_apic.eoi();
        }
        self.io_apic.synchronize();
        let hpet_counter = (time::get_ticks() as u64).wrapping_mul(14_318);
        self.npt
            .set_slice(0xfed0_00f0, &hpet_counter.to_le_bytes())?;
        if self.legacy_timer.pit.poll() {
            self.legacy_timer.pic.raise_irq(0);
            self.io_apic.set_irq_level(0, true);
        }
        self.sync_uart_irq();
        if let Some(device) = self.passthrough.as_mut() {
            let (irq, asserted) = device.poll_interrupt_level();
            self.legacy_timer.pic.set_irq_level(irq, asserted);
            self.io_apic.set_irq_level(irq, asserted);
        }
        let (irq, asserted) = self.guest_block.interrupt_level();
        self.legacy_timer.pic.set_irq_level(irq, asserted);
        self.io_apic.set_irq_level(irq, asserted);
        let next_irq = self.legacy_timer.pic.next_irq();
        let local_vector = self.local_apic.pending_timer_vector();
        let msi_vector = self.guest_block.pending_msi_vector();
        let io_interrupt = self.io_apic.pending_vector();

        let vmcb = self.vmcb.get_raw_vmcb();
        let interrupts_enabled = vmcb.state_save_area.rflags & (1 << 9) != 0;
        let in_interrupt_shadow = vmcb
            .control_area
            .interrupt_shadow_flags
            .contains(InterruptShadowFlags::INTERRUPT_SHADOW);
        let event_already_pending = vmcb.control_area.event_injection & (1 << 31) != 0;
        if local_vector.is_none()
            && msi_vector.is_none()
            && io_interrupt.is_none()
            && next_irq.is_none()
        {
            return Ok(());
        }
        if !interrupts_enabled || event_already_pending {
            return Ok(());
        }

        // SVM reports intercepted HLT before executing it. For STI;HLT, the
        // VMCB therefore still carries STI's interrupt shadow. Completing the
        // HLT in software retires that shadow, so a pending IRQ can be
        // injected immediately instead of re-entering the same HLT forever.
        if in_interrupt_shadow && !self.halted {
            return Ok(());
        }

        if self.halted {
            self.advance_guest_rip()?;
            self.halted = false;
            self.vmcb
                .get_raw_vmcb()
                .control_area
                .interrupt_shadow_flags
                .remove(InterruptShadowFlags::INTERRUPT_SHADOW);
        }

        let vector = if let Some(vector) = local_vector {
            vector
        } else if let Some(vector) = msi_vector {
            vector
        } else if let Some((_, vector)) = io_interrupt {
            vector
        } else if next_irq.unwrap() < 8 {
            self.legacy_timer
                .pic
                .master_base
                .wrapping_add(next_irq.unwrap())
        } else {
            self.legacy_timer
                .pic
                .slave_base
                .wrapping_add(next_irq.unwrap() - 8)
        };
        const EVENT_VALID: u64 = 1 << 31;
        let control = &mut self.vmcb.get_raw_vmcb().control_area;
        control.event_injection = vector as u64 | EVENT_VALID;
        control.vmcb_clean_bits = 0;
        let level_triggered =
            io_interrupt.is_some_and(|(irq, _)| self.io_apic.is_level_triggered(irq));
        self.local_apic.accept_interrupt(vector, level_triggered);
        if local_vector.is_some() {
            self.local_apic.acknowledge_timer();
        } else if msi_vector.is_some() {
            self.guest_block.acknowledge_msi();
        } else if let Some((irq, _)) = io_interrupt {
            // Delivery consumes the pending edge or sets remote-IRR for a
            // level input. A still-asserted level is offered again only after
            // the guest writes local-APIC EOI.
            self.io_apic.acknowledge(irq);
        } else if let Some(irq) = next_irq {
            self.legacy_timer.pic.acknowledge(irq);
        }
        Ok(())
    }

    fn preserve_interrupted_event(&mut self) -> Result<bool, &'static str> {
        const EVENT_VALID: u64 = 1 << 31;

        let control = &mut self.vmcb.get_raw_vmcb().control_area;
        let interrupted_event = control.exit_int_info;
        if interrupted_event & EVENT_VALID == 0 {
            return Ok(false);
        }
        // EXITINTINFO describes an event whose delivery was interrupted by
        // this VMEXIT. SVM does not queue it automatically: copying the full
        // field preserves vector, type, error-code-valid, and error code for
        // the next VMRUN. Losing an injected IRQ0 here would leave the virtual
        // PIC in-service forever because the guest never reaches its handler
        // and EOI.
        let pending_event = control.event_injection;
        if pending_event & EVENT_VALID != 0 && pending_event != interrupted_event {
            return Err("AMD VMEXIT returned an event while another injection was pending");
        }
        control.event_injection = interrupted_event;
        control.vmcb_clean_bits = 0;
        Ok(true)
    }

    fn inject_exception(&mut self, vector: u8, error_code: Option<u32>) {
        const EVENT_TYPE_EXCEPTION: u64 = 3;
        const EVENT_ERROR_CODE_VALID: u64 = 1 << 11;
        const EVENT_VALID: u64 = 1 << 31;

        let mut event = vector as u64 | (EVENT_TYPE_EXCEPTION << 8) | EVENT_VALID;
        if let Some(error_code) = error_code {
            event |= EVENT_ERROR_CODE_VALID | ((error_code as u64) << 32);
        }

        let control = &mut self.vmcb.get_raw_vmcb().control_area;
        control.event_injection = event;
        control.vmcb_clean_bits = 0;
    }

    fn handle_io(&mut self, block: Option<&mut VirtioBlock>) -> Result<(), &'static str> {
        let exit_info = self.vmcb.get_raw_vmcb().control_area.exit_info1;
        let is_input = exit_info & 1 != 0;
        let is_string = exit_info & (1 << 2) != 0;
        let is_rep = exit_info & (1 << 3) != 0;
        let size_bits = (exit_info >> 4) & 0x7;
        let port = ((exit_info >> 16) & 0xffff) as u16;

        let size = match size_bits {
            1 => 1,
            2 => 2,
            4 => 4,
            _ => return Err("AMD guest attempted I/O with invalid operand size"),
        };

        if is_string || is_rep {
            if !is_input || port != 0x511 || size != 1 {
                return Err("AMD guest attempted unsupported string I/O");
            }
            let count = if is_rep { self.guest_registers.rcx } else { 1 };
            if count > self.guest_memory_size {
                return Err("AMD guest fw_cfg transfer is too large");
            }
            let decrement = self.vmcb.get_raw_vmcb().state_save_area.rflags & (1 << 10) != 0;
            for _ in 0..count {
                let address =
                    self.vmcb.get_raw_vmcb().state_save_area.es.base + self.guest_registers.rdi;
                let value = self.fw_cfg.read_u8(self.guest_memory_size);
                self.npt.set(address, value)?;
                self.guest_registers.rdi = if decrement {
                    self.guest_registers.rdi.wrapping_sub(1)
                } else {
                    self.guest_registers.rdi.wrapping_add(1)
                };
            }
            if is_rep {
                self.guest_registers.rcx = 0;
            }
            return self.advance_guest_rip();
        }

        let is_config_address = (0x0cf8..=0x0cfb).contains(&port);
        if is_config_address {
            if is_input {
                let value = self.guest_block.io_in(port, size, 0);
                let mask = match size {
                    1 => u8::MAX as u64,
                    2 => u16::MAX as u64,
                    _ => u64::MAX,
                };
                let rax = &mut self.vmcb.get_raw_vmcb().state_save_area.rax;
                *rax = (*rax & !mask) | u64::from(value);
            } else {
                let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u32;
                self.guest_block.write_config_address(port, size, value);
                if let Some(device) = self.passthrough.as_mut() {
                    device.io_out(port, size, value);
                }
            }
            return self.advance_guest_rip();
        }

        if self.guest_block.handles_port(port) {
            let Some(block) = block else {
                return Err("VM virtio-blk backend is unavailable");
            };
            if is_input {
                let value = u64::from(self.guest_block.io_in(port, size, block.capacity_sectors()));
                let mask = match size {
                    1 => u8::MAX as u64,
                    2 => u16::MAX as u64,
                    _ => u64::MAX,
                };
                let rax = &mut self.vmcb.get_raw_vmcb().state_save_area.rax;
                *rax = (*rax & !mask) | value;
            } else {
                let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u32;
                self.guest_block
                    .io_out(port, size, value, &mut self.npt, block)?;
            }
            return self.advance_guest_rip();
        }

        if self
            .passthrough
            .as_ref()
            .is_some_and(|device| device.handles_port(port))
        {
            let device = self.passthrough.as_mut().unwrap();
            if is_input {
                let value = u64::from(device.io_in(port, size));
                let mask = match size {
                    1 => u8::MAX as u64,
                    2 => u16::MAX as u64,
                    _ => u64::MAX,
                };
                let rax = &mut self.vmcb.get_raw_vmcb().state_save_area.rax;
                *rax = (*rax & !mask) | value;
            } else {
                let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u32;
                device.io_out(port, size, value);
            }
            return self.advance_guest_rip();
        }

        if is_input {
            let value = match (port, size) {
                (0x40, 1) => self.legacy_timer.pit.read() as u32,
                (0x42, 1) => self.legacy_timer.pit_channel2.read() as u32,
                (0x61, 1) => self.legacy_timer.speaker_status() as u32,
                (0x70, 1) => self.rtc.read_selector() as u32,
                (0x71, 1) => self.rtc.read_data() as u32,
                (0x511, 1) => self.fw_cfg.read_u8(self.guest_memory_size) as u32,
                (0x608, 4) | (0xb008, 4) => self.pm_timer.read(),
                (0xb000 | 0xb002 | 0xb004, 2 | 4) => {
                    self.pm_registers.read(port, size).unwrap_or(0)
                }
                (0x20 | 0x21 | 0xa0 | 0xa1, 1) => self.legacy_timer.pic.read(port) as u32,
                (0x3f8..=0x3ff, 1) => self.serial_in(port) as u32,
                (_, 1) => u8::MAX as u32,
                (_, 2) => u16::MAX as u32,
                (_, 4) => u32::MAX,
                _ => unreachable!(),
            };
            let mask = match size {
                1 => u8::MAX as u64,
                2 => u16::MAX as u64,
                // A 32-bit register write clears the upper half of RAX.
                4 => u64::MAX,
                _ => unreachable!(),
            };
            let rax = &mut self.vmcb.get_raw_vmcb().state_save_area.rax;
            *rax = (*rax & !mask) | value as u64;
        } else if self.pm_registers.write(
            port,
            size,
            self.vmcb.get_raw_vmcb().state_save_area.rax as u32,
        ) {
        } else if size == 1 && port == 0x40 {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u8;
            self.legacy_timer.pit.write(value);
        } else if size == 1 && port == 0x42 {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u8;
            self.legacy_timer.pit_channel2.write(value);
        } else if size == 1 && port == 0x43 {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u8;
            self.legacy_timer.write_pit_control(value);
        } else if size == 1 && port == 0x61 {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u8;
            self.legacy_timer.write_speaker_control(value);
        } else if size == 1 && port == 0x70 {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u8;
            self.rtc.select(value);
        } else if size == 1 && port == 0x71 {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u8;
            self.rtc.write_data(value);
        } else if size == 1 && matches!(port, 0x20 | 0x21 | 0xa0 | 0xa1) {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u8;
            self.legacy_timer.pic.write(port, value);
        } else if size == 1 && (0x3f8..=0x3ff).contains(&port) {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u8;
            self.serial_out(port, value);
        } else if size == 2 && port == 0x510 {
            let value = self.vmcb.get_raw_vmcb().state_save_area.rax as u16;
            self.fw_cfg.select(value);
        }

        if size == 1 && (0x3f8..=0x3ff).contains(&port) {
            self.sync_uart_irq();
        }

        self.advance_guest_rip()
    }

    fn serial_in(&mut self, port: u16) -> u8 {
        match port {
            0x3f8 if self.serial.lcr & 0x80 != 0 => self.serial.divisor_low,
            0x3f8 => self.serial.dequeue_rx(),
            0x3f9 if self.serial.lcr & 0x80 != 0 => self.serial.divisor_high,
            0x3f9 => self.serial.ier,
            0x3fa => self.serial.interrupt_identification(),
            0x3fb => self.serial.lcr,
            0x3fc => self.serial.mcr,
            0x3fd => self.serial.line_status(),
            0x3fe => self.serial.modem_status(),
            0x3ff => self.serial.scratch,
            _ => u8::MAX,
        }
    }

    fn serial_out(&mut self, port: u16, value: u8) {
        match port {
            0x3f8 if self.serial.lcr & 0x80 != 0 => self.serial.divisor_low = value,
            0x3f8 => {
                if self.serial.mcr & 0x10 != 0 {
                    self.serial.enqueue_rx(value);
                } else {
                    serial::write_guest_raw_byte(value);
                }
                self.serial.thre_interrupt_pending = self.serial.ier & 0x02 != 0;
            }
            0x3f9 if self.serial.lcr & 0x80 != 0 => self.serial.divisor_high = value,
            0x3f9 => self.serial.write_ier(value),
            0x3fa => self.serial.write_fcr(value),
            0x3fb => self.serial.lcr = value,
            0x3fc => self.serial.mcr = value & 0x1f,
            0x3ff => self.serial.scratch = value,
            _ => {}
        }
    }

    fn handle_msr(&mut self) -> Result<(), &'static str> {
        const EFER: u32 = 0xc000_0080;
        const APIC_BASE: u32 = 0x1b;
        const STAR: u32 = 0xc000_0081;
        const LSTAR: u32 = 0xc000_0082;
        const CSTAR: u32 = 0xc000_0083;
        const SFMASK: u32 = 0xc000_0084;
        const FS_BASE: u32 = 0xc000_0100;
        const GS_BASE: u32 = 0xc000_0101;
        const KERNEL_GS_BASE: u32 = 0xc000_0102;
        const SYSENTER_CS: u32 = 0x174;
        const SYSENTER_ESP: u32 = 0x175;
        const SYSENTER_EIP: u32 = 0x176;
        const PAT: u32 = 0x277;
        const PATCH_LEVEL: u32 = 0x8b;
        const TSC_AUX: u32 = 0xc000_0103;
        const AMD64_NB_CFG: u32 = 0xc001_001f;

        let is_write = self.vmcb.get_raw_vmcb().control_area.exit_info1 & 1 != 0;
        let index = self.guest_registers.rcx as u32;
        if is_write {
            let value = (self.guest_registers.rdx as u32 as u64) << 32
                | self.vmcb.get_raw_vmcb().state_save_area.rax as u32 as u64;
            let state = &mut self.vmcb.get_raw_vmcb().state_save_area;
            match index {
                APIC_BASE => self.apic_base = value & 0xffff_f000 | (value & 0xd00),
                0x802..=0x83f => {
                    if self.local_apic.write_x2apic(index, value).is_none() {
                        self.inject_exception(13, Some(0));
                        return Ok(());
                    }
                    if index == 0x80b {
                        if self.local_apic.eoi() {
                            self.io_apic.eoi();
                        }
                    }
                }
                EFER => state.efer = value | (1 << 12),
                STAR => state.star = value,
                LSTAR => state.lstar = value,
                CSTAR => state.cstar = value,
                SFMASK => state.sfmask = value,
                FS_BASE => state.fs.base = value,
                GS_BASE => state.gs.base = value,
                KERNEL_GS_BASE => state.kernel_gs_base = value,
                SYSENTER_CS => state.sysenter_cs = value,
                SYSENTER_ESP => state.sysenter_esp = value,
                SYSENTER_EIP => state.sysenter_eip = value,
                PAT => match common::msr::validate_pat(value) {
                    Ok(value) => state.g_pat = value,
                    Err(_) => {
                        self.inject_exception(13, Some(0));
                        return Ok(());
                    }
                },
                TSC_AUX => self.tsc_aux = value,
                // Linux uses bit 46 to enable extended CF8 PCI config-space
                // addressing on AMD systems. PCI is hidden from this guest,
                // but preserving the virtual bit avoids an unchecked #GP.
                AMD64_NB_CFG => self.nb_cfg = value & (1 << 46),
                _ => {
                    info!(
                        "Unsupported AMD guest WRMSR: index={:#x} value={:#x}",
                        index, value
                    );
                    self.inject_exception(13, Some(0));
                    return Ok(());
                }
            }
        } else {
            let state = &self.vmcb.get_raw_vmcb().state_save_area;
            let value = match index {
                APIC_BASE => self.apic_base,
                0x802..=0x83f => match self.local_apic.read_x2apic(index) {
                    Some(value) => value,
                    None => {
                        self.inject_exception(13, Some(0));
                        return Ok(());
                    }
                },
                EFER => state.efer,
                STAR => state.star,
                LSTAR => state.lstar,
                CSTAR => state.cstar,
                SFMASK => state.sfmask,
                FS_BASE => state.fs.base,
                GS_BASE => state.gs.base,
                KERNEL_GS_BASE => state.kernel_gs_base,
                SYSENTER_CS => state.sysenter_cs,
                SYSENTER_ESP => state.sysenter_esp,
                SYSENTER_EIP => state.sysenter_eip,
                PAT => state.g_pat,
                // The physical family/model is exposed to the guest, so the
                // matching host patch revision is the only consistent
                // read-only value. It is captured once when the VCPU is
                // created; guest accesses never pass through to the host MSR.
                PATCH_LEVEL => self.host_patch_level,
                TSC_AUX => self.tsc_aux,
                AMD64_NB_CFG => self.nb_cfg,
                _ => {
                    info!("Unsupported AMD guest RDMSR: {:#x}", index);
                    self.inject_exception(13, Some(0));
                    return Ok(());
                }
            };
            self.vmcb.get_raw_vmcb().state_save_area.rax = value as u32 as u64;
            self.guest_registers.rdx = (value >> 32) as u32 as u64;
        }

        self.advance_guest_rip()
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
    fn prepare(&mut self, frame_allocator: &mut BitmapMemoryTable) -> Result<(), &'static str> {
        if !self.initialized {
            self.setup(frame_allocator)?;
            self.initialized = true;
        }
        Ok(())
    }

    fn run(
        &mut self,
        frame_allocator: &mut BitmapMemoryTable,
        block: Option<&mut VirtioBlock>,
    ) -> Result<(), &'static str> {
        interrupts::without_interrupts(|| {
            let result = (|| unsafe {
                self.prepare(frame_allocator)?;

                {
                    let vmcb = self.vmcb.get_raw_vmcb();
                    vmcb.control_area.exit_code = 0;
                    vmcb.control_area.exit_info1 = 0;
                    vmcb.control_area.exit_info2 = 0;
                    vmcb.control_area.exit_int_info = 0;
                }

                self.poll_host_serial();
                self.prepare_device_interrupt()?;
                if self.halted
                    && self.vmcb.get_raw_vmcb().control_area.event_injection & (1 << 31) == 0
                {
                    // Avoid repeatedly entering an intercepted HLT while no event
                    // is deliverable. Returning restores host interrupts between
                    // bounded device polls.
                    return Ok(());
                }

                write_msr(0xC001_0117, self.hsave.start_address().as_u64());

                self.host_xsave_state.save_host_and_load_guest(3)?;

                super::asm::asm_vmrun(
                    self.vmcb.frame.start_address().as_u64(),
                    &mut self.guest_registers,
                    &mut self.host_fx_state,
                    &mut self.guest_fx_state,
                    self.host_xsave_addr,
                    self.host_xsave_mask,
                    self.host_vmcb_state.start_address().as_u64(),
                );

                // A setup/reset flush request must remain set until VMRUN has
                // consumed it. Clearing this before entry can reuse stale NPT
                // translations for a recycled ASID.
                self.vmcb.get_raw_vmcb().control_area.tlb_control = 0;

                let interrupted_event = self.preserve_interrupted_event()?;
                // SVM leaves EVENTINJ unchanged after a successful VMRUN.
                // EXITINTINFO above is the authoritative indication that
                // delivery was interrupted; otherwise the event was consumed
                // and retaining VALID would inject it again on every entry.
                if !interrupted_event {
                    self.vmcb.get_raw_vmcb().control_area.event_injection = 0;
                }

                let vmcb = self.vmcb.get_raw_vmcb();
                let exit_code = vmcb.control_area.exit_code;
                if !self.exit_reported
                    || !matches!(exit_code, 0x60 | 0x72 | 0x77 | 0x78 | 0x7b | 0x7c | 0x89)
                {
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
                    // The pending physical vector is delivered to the host once
                    // GIF and IF are restored below. Guest RIP is unchanged.
                    0x60 => Ok(()),
                    0x72 => {
                        self.handle_cpuid();
                        Ok(())
                    }
                    0x77 => self.advance_guest_rip(),
                    0x78 => {
                        // Poll on the intercepted HLT until an interrupt is
                        // deliverable. IRQ injection advances to NRIP.
                        self.halted = true;
                        Ok(())
                    }
                    0x7b => self.handle_io(block),
                    0x7c => self.handle_msr(),
                    // Guest firmware uses WBINVD while publishing its memory
                    // map. Guest RAM is coherent host memory, so it is safe to
                    // complete this as a no-op.
                    0x89 => self.advance_guest_rip(),
                    0x8c => Err("AMD guest attempted unsupported XSETBV"),
                    0x7f => Err("AMD guest shutdown (likely a triple fault)"),
                    0x400 => {
                        let guest_address = self.vmcb.get_raw_vmcb().control_area.exit_info2;
                        if self.npt.get_phys_addr(guest_address).is_some() {
                            Err("AMD NPT permission fault")
                        } else {
                            self.allocate_guest_memory_chunk(guest_address, frame_allocator)?;
                            self.vmcb.get_raw_vmcb().control_area.tlb_control = 3;
                            Ok(())
                        }
                    }
                    u32::MAX => Err("VMRUN rejected the VMCB guest state"),
                    _ => Err("Unhandled AMD VMEXIT"),
                }
            })();

            // VMRUN sets GIF while the guest executes and VMEXIT clears it.
            // Restore GIF before `without_interrupts` restores IF: enabling IF
            // first leaves a window where a pending LAPIC timer cannot be
            // delivered and can strand the management loop after `vm stop`.
            unsafe {
                core::arch::asm!("stgi", options(nomem, nostack, preserves_flags));
            }
            result
        })
    }

    fn write_memory(&mut self, addr: u64, data: u8) -> Result<(), &'static str> {
        self.npt.set(addr, data)
    }

    fn reset(&mut self) -> Result<(), &'static str> {
        if self.guest_memory_initialization_failed {
            return Err("Guest memory initialization previously failed");
        }
        if let Some(device) = self.passthrough.as_mut() {
            device.reset();
        }
        self.guest_block.reset();
        self.fw_cfg.reset();
        self.pm_timer.reset();
        self.pm_registers.reset();
        let tsc_khz = crate::interrupt::apic::GUEST_TSC_KHZ
            .get()
            .copied()
            .ok_or("TSC frequency unavailable for AMD VCPU reset")?;
        self.initialized = false;
        self.exit_reported = false;
        self.serial = SerialState::default();
        self.rtc = RtcState::new(self.guest_memory_size);
        self.legacy_timer = LegacyTimer::new(tsc_khz);
        self.halted = false;
        self.tsc_aux = 0;
        self.apic_base = 0xfee0_0900;
        self.local_apic.reset();
        self.io_apic.reset();
        self.nb_cfg = 0;
        self.guest_registers = GuestRegisters::default();
        self.guest_fx_state = FxState::guest_default();
        unsafe {
            core::ptr::write_bytes(self.vmcb.frame.start_address().as_u64() as *mut u8, 0, 4096);
        }
        Ok(())
    }

    fn destroy(self: Box<Self>, frame_allocator: &mut BitmapMemoryTable) {
        let AMDVCpu {
            vmcb,
            hsave,
            host_vmcb_state,
            npt,
            permission_maps,
            host_xsave_state,
            guest_memory_size,
            ..
        } = *self;
        npt.reclaim(frame_allocator, guest_memory_size);
        permission_maps.reclaim(frame_allocator);
        host_xsave_state.reclaim(frame_allocator);
        frame_allocator.deallocate_frame(vmcb.frame);
        frame_allocator.deallocate_frame(hsave);
        frame_allocator.deallocate_frame(host_vmcb_state);
    }

    fn is_idle(&self) -> bool {
        self.halted
            && self.vmcb.get_raw_vmcb().control_area.event_injection & (1 << 31) == 0
            && !self.local_apic.has_pending_timer()
            && self.guest_block.pending_msi_vector().is_none()
            && !self.io_apic.has_pending_interrupt()
            && self.legacy_timer.pic.next_irq().is_none()
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

    fn get_allocated_guest_memory_size(&self) -> u64 {
        self.guest_memory_allocated
    }

    fn get_used_guest_memory_size(&self) -> u64 {
        common::uefi::ram_ranges(self.guest_memory_size)
            .into_iter()
            .map(|(base, size)| self.npt.accessed_bytes(base, base.saturating_add(size)))
            .sum()
    }

    fn new(
        frame_allocator: &mut BitmapMemoryTable,
        hardware_vcpu_id: usize,
        guest_memory_size: u64,
        passthrough: Option<PassthroughDescriptor>,
    ) -> Result<Self, &'static str>
    where
        Self: Sized,
    {
        let guest_asid = hardware_vcpu_id
            .checked_add(1)
            .and_then(|id| u32::try_from(id).ok())
            .ok_or("AMD SVM ASID space is exhausted")?;
        let asid_count = cpuid!(0x8000_000a).ebx;
        if guest_asid >= asid_count {
            return Err("CPU does not provide enough AMD SVM ASIDs for another VM");
        }

        // FXSAVE64/FXRSTOR64 are used around every VMRUN to isolate x87,
        // MXCSR, and XMM state. Make the host prerequisite explicit before
        // the first assembly entry.
        unsafe {
            Cr4::write(Cr4::read() | Cr4Flags::OSFXSR);
        }

        let mut efer = common::read_msr(0xc000_0080);
        efer |= 1 << 12;
        common::write_msr(0xc000_0080, efer);

        let hsave = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate frame for VCPU HSave area")?;
        unsafe {
            core::ptr::write_bytes(hsave.start_address().as_u64() as *mut u8, 0, 4096);
        }
        let host_vmcb_state = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate frame for AMD host VMCB state")?;
        unsafe {
            core::ptr::write_bytes(host_vmcb_state.start_address().as_u64() as *mut u8, 0, 4096);
        }

        let permission_maps = PermissionMaps::new(frame_allocator)?;
        let host_xsave_state = HostXsaveState::new(frame_allocator)?;
        let host_xsave_addr = host_xsave_state.addr();
        let host_xsave_mask = host_xsave_state.mask();
        let host_patch_level = common::read_msr(0x8b);
        let tsc_khz = crate::interrupt::apic::GUEST_TSC_KHZ
            .get()
            .copied()
            .ok_or("TSC frequency unavailable for AMD legacy timer")?;
        let passthrough = passthrough
            .map(|descriptor| PassthroughNic::new(descriptor, frame_allocator))
            .transpose()?;

        Ok(AMDVCpu {
            initialized: false,
            guest_memory_initialized: false,
            guest_memory_initialization_failed: false,
            exit_reported: false,
            vmcb: Vmcb::new(frame_allocator)?,
            hsave,
            host_vmcb_state,
            npt: Npt::new(frame_allocator)?,
            permission_maps,
            serial: SerialState::default(),
            rtc: RtcState::new(guest_memory_size),
            legacy_timer: LegacyTimer::new(tsc_khz),
            halted: false,
            tsc_aux: 0,
            apic_base: 0xfee0_0900,
            local_apic: common::local_apic::LocalApic::new(),
            io_apic: common::io_apic::IoApic::new(),
            nb_cfg: 0,
            host_patch_level,
            guest_registers: GuestRegisters::default(),
            host_fx_state: FxState::zeroed(),
            guest_fx_state: FxState::guest_default(),
            host_xsave_addr,
            host_xsave_mask,
            host_xsave_state,
            guest_memory_size,
            guest_memory_allocated: 0,
            // ASID zero is reserved. A unique ASID keeps cached nested
            // translations isolated when VMs are scheduled round-robin.
            guest_asid,
            passthrough,
            guest_block: GuestVirtioBlock::new(),
            fw_cfg: common::fw_cfg::FwCfg::new(),
            pm_timer: common::timer::AcpiPmTimer::new(),
            pm_registers: common::timer::AcpiPmRegisters::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uart_rx_ring_is_bounded_and_reports_overrun() {
        let mut uart = SerialState::default();
        for value in 0..=u8::MAX {
            uart.enqueue_rx(value);
        }
        uart.enqueue_rx(0xaa);
        assert_eq!(uart.rx_len, 256);
        assert_eq!(uart.rx_remaining(), 0);
        assert!(uart.overrun);
        assert_eq!(uart.dequeue_rx(), 0);
        assert_eq!(uart.rx_remaining(), 1);
        for value in 1..=u8::MAX {
            assert_eq!(uart.dequeue_rx(), value);
        }
        assert_eq!(uart.rx_len, 0);
    }

    #[test]
    fn uart_prioritizes_rx_and_clears_thre_on_iir_read() {
        let mut uart = SerialState::default();
        uart.write_ier(0x03);
        uart.enqueue_rx(b'x');
        assert_eq!(uart.interrupt_identification() & 0x0f, 0x04);
        assert_eq!(uart.dequeue_rx(), b'x');
        assert_eq!(uart.interrupt_identification() & 0x0f, 0x02);
        assert_eq!(uart.interrupt_identification() & 0x0f, 0x01);
    }

    #[test]
    fn pic_prioritizes_irq0_and_specific_eoi_releases_irq4() {
        let mut pic = PicState::new();
        pic.master_mask = !(1 << 0 | 1 << 4);
        pic.raise_irq(4);
        pic.raise_irq(0);
        assert_eq!(pic.next_irq(), Some(0));
        pic.acknowledge(0);
        assert_eq!(pic.next_irq(), None);
        pic.write(0x20, 0x60);
        assert_eq!(pic.next_irq(), Some(4));
    }
}
