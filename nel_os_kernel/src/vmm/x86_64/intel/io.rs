use core::sync::atomic::{AtomicU16, Ordering};

use x86::vmx::{self, vmcs};
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

use super::qual::QualIo;
use crate::{
    info,
    interrupt::{idt::IRQ_TIMER, subscriber::InterruptContext},
    serial,
    vmm::x86_64::intel::{
        register::GuestRegisters, vmcs::controls::EntryIntrInfo, vmread, vmwrite,
    },
};

fn interrupt_vector_to_irq(vector: u8) -> Option<u8> {
    if vector == IRQ_TIMER as u8 {
        // The host timer only preempts the current VM. Each guest receives
        // IRQ0 from its own emulated PIT, not from the host LAPIC cadence.
        return None;
    }

    if (0x20..0x30).contains(&vector) {
        return Some(vector - 0x20);
    }

    None
}

pub fn vmm_interrupt_subscriber(pending_ptr: *mut core::ffi::c_void, context: &InterruptContext) {
    if pending_ptr.is_null() {
        return;
    }

    let Some(irq) = interrupt_vector_to_irq(context.vector) else {
        return;
    };
    debug_assert!(irq < u16::BITS as u8);

    // The callback may preempt normal VCPU work. Only touch this atomic
    // interrupt latch here; materializing `&mut IntelVCpu` from the subscriber
    // context would alias the main loop's mutable reference.
    let pending = unsafe { &*(pending_ptr as *const AtomicU16) };
    pending.fetch_or(1u16 << irq, Ordering::Release);
}

#[cfg(test)]
mod interrupt_vector_tests {
    use super::{Pic, Ps2Controller, QualIo, Serial, interrupt_vector_to_irq};
    use crate::vmm::x86_64::intel::register::GuestRegisters;

    #[test]
    fn maps_pic_and_host_timer_vectors_without_overflow() {
        assert_eq!(interrupt_vector_to_irq(0x20), Some(0));
        assert_eq!(interrupt_vector_to_irq(0x2f), Some(15));
        assert_eq!(interrupt_vector_to_irq(0x30), None);
        assert_eq!(interrupt_vector_to_irq(0x31), None);
    }

    #[test]
    fn uart_rx_capacity_tracks_dequeues() {
        let mut uart = Serial::default();
        for value in 0..=u8::MAX {
            uart.enqueue(value);
        }
        assert_eq!(uart.remaining(), 0);
        assert_eq!(uart.dequeue(), 0);
        assert_eq!(uart.remaining(), 1);
    }

    #[test]
    fn uart_transmit_reasserts_thre_interrupt() {
        let mut pic = Pic::new(1, 256 * 1024 * 1024);
        let mut registers = GuestRegisters::default();
        registers.rax = b'x' as u64;
        pic.serial.ier = 0b10;
        pic.serial.mcr = 0b1000;

        pic.handle_serial_out(&mut registers, QualIo::from(0x03f8u64 << 16));

        assert_ne!(pic.pending_irq & (1 << 4), 0);
        assert_eq!(pic.serial.interrupt_identification() & 0x0f, 0x02);
    }

    #[test]
    fn pending_pic_irq_requires_another_vcpu_entry() {
        let mut pic = Pic::new(1, 256 * 1024 * 1024);
        assert!(!pic.has_pending_interrupt());

        pic.pending_irq |= 1 << 4;

        assert!(pic.has_pending_interrupt());
    }

    #[test]
    fn ps2_controller_completes_firmware_probe_without_timeout() {
        let mut controller = Ps2Controller::new();

        controller.write_command(0xaa);
        assert_ne!(controller.status() & 1, 0);
        assert_eq!(controller.read_data(), 0x55);

        controller.write_command(0xab);
        assert_ne!(controller.status() & 1, 0);
        assert_eq!(controller.read_data(), 0x00);

        controller.write_data(0xf4);
        assert_eq!(controller.read_data(), 0xfa);
        controller.write_data(0xff);
        assert_eq!(controller.read_data(), 0xfa);
        assert_eq!(controller.read_data(), 0xaa);
        assert_eq!(controller.status() & 1, 0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitPhase {
    Uninitialized,
    Phase1,
    Phase2,
    Phase3,
    Initialized,
}

pub enum ReadSel {
    Irr,
    Isr,
}

#[derive(Debug, Clone, Copy)]
pub struct Serial {
    pub ier: u8,
    pub mcr: u8,
    lcr: u8,
    scratch: u8,
    divisor_low: u8,
    divisor_high: u8,
    fifo_enabled: bool,
    thre_interrupt_pending: bool,
    rx: [u8; 256],
    rx_head: usize,
    rx_len: usize,
    overrun: bool,
}

impl Default for Serial {
    fn default() -> Self {
        Self {
            ier: 0,
            mcr: 0,
            lcr: 0,
            scratch: 0,
            divisor_low: 0,
            divisor_high: 0,
            fifo_enabled: false,
            thre_interrupt_pending: false,
            rx: [0; 256],
            rx_head: 0,
            rx_len: 0,
            overrun: false,
        }
    }
}

impl Serial {
    fn enqueue(&mut self, byte: u8) {
        if self.rx_len == self.rx.len() {
            self.overrun = true;
            return;
        }
        let tail = (self.rx_head + self.rx_len) % self.rx.len();
        self.rx[tail] = byte;
        self.rx_len += 1;
    }

    fn dequeue(&mut self) -> u8 {
        if self.rx_len == 0 {
            return 0;
        }
        let byte = self.rx[self.rx_head];
        self.rx_head = (self.rx_head + 1) % self.rx.len();
        self.rx_len -= 1;
        byte
    }

    fn remaining(&self) -> usize {
        self.rx.len() - self.rx_len
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
            self.rx_head = 0;
            self.rx_len = 0;
            self.overrun = false;
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
            self.thre_interrupt_pending = false;
            0x02
        } else {
            0x01
        };
        fifo | reason
    }

    fn interrupt_pending(&self) -> bool {
        (self.overrun && self.ier & 0x04 != 0)
            || (self.rx_len != 0 && self.ier & 0x01 != 0)
            || (self.thre_interrupt_pending && self.ier & 0x02 != 0)
    }
}

#[derive(Debug, Clone, Copy)]
struct RtcState {
    selector: u8,
    registers: [u8; 128],
}

#[derive(Debug, Clone, Copy)]
struct Ps2Controller {
    output: [u8; 4],
    output_head: usize,
    output_len: usize,
    config: u8,
    expect_config: bool,
}

impl Ps2Controller {
    fn new() -> Self {
        Self {
            output: [0; 4],
            output_head: 0,
            output_len: 0,
            config: 0,
            expect_config: false,
        }
    }

    fn enqueue(&mut self, value: u8) {
        if self.output_len == self.output.len() {
            return;
        }
        let tail = (self.output_head + self.output_len) % self.output.len();
        self.output[tail] = value;
        self.output_len += 1;
    }

    fn status(&self) -> u8 {
        // Bit 2 reports that the controller self-test completed. The input
        // buffer always drains synchronously; bit 0 follows the response FIFO.
        0x04 | u8::from(self.output_len != 0)
    }

    fn read_data(&mut self) -> u8 {
        if self.output_len == 0 {
            return 0;
        }
        let value = self.output[self.output_head];
        self.output_head = (self.output_head + 1) % self.output.len();
        self.output_len -= 1;
        value
    }

    fn write_command(&mut self, command: u8) {
        match command {
            0x20 => self.enqueue(self.config),
            0x60 => self.expect_config = true,
            0xaa => self.enqueue(0x55),
            0xa9 | 0xab => self.enqueue(0x00),
            _ => {}
        }
    }

    fn write_data(&mut self, value: u8) {
        if self.expect_config {
            self.config = value;
            self.expect_config = false;
        } else if value == 0xff {
            self.enqueue(0xfa);
            self.enqueue(0xaa);
        } else {
            // Firmware only needs command acknowledgement and reset/BAT
            // completion. Guest input remains on the emulated serial port.
            self.enqueue(0xfa);
        }
    }
}

impl RtcState {
    fn new(guest_memory_size: u64) -> Self {
        let mut registers = [0; 128];
        // Fixed, valid BCD timestamp: 2000-01-01 00:00:00 (Saturday).
        registers[0x06] = 0x07;
        registers[0x07] = 0x01;
        registers[0x08] = 0x01;
        registers[0x09] = 0x00;
        registers[0x0a] = 0x26; // UIP clear, 32-kHz divider.
        registers[0x0b] = 0x02; // BCD, 24-hour mode, interrupts disabled.
        registers[0x0c] = 0x00;
        registers[0x0d] = 0x80; // Valid RAM/time (VRT).
        registers[0x32] = 0x20; // Conventional BCD century byte.
        registers[0x15] = 0x80; // 640 KiB, little-endian.
        registers[0x16] = 0x02;
        registers[0x17] = 0x00; // 15 MiB between 1 MiB and 16 MiB.
        registers[0x18] = 0x3c;
        registers[0x30] = registers[0x17];
        registers[0x31] = registers[0x18];
        let above_16m = guest_memory_size
            .saturating_sub(16 * 1024 * 1024)
            .div_ceil(64 * 1024)
            .min(u16::MAX as u64) as u16;
        registers[0x34] = above_16m as u8;
        registers[0x35] = (above_16m >> 8) as u8;
        Self {
            selector: 0,
            registers,
        }
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

#[derive(Debug, Clone, Copy)]
struct PitChannel {
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

impl PitChannel {
    const PIT_HZ: u64 = 1_193_182;

    fn new(tsc_khz: u64) -> Self {
        Self {
            tsc_hz: tsc_khz.saturating_mul(1_000),
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
        let now = Self::rdtsc();
        if !self.armed || now < self.deadline {
            return false;
        }
        if matches!(self.mode, 2 | 3) {
            let period = self.period_cycles();
            let elapsed = now.wrapping_sub(self.deadline);
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

pub struct Pic {
    pub primary_mask: u8,
    pub secondary_mask: u8,
    pub primary_phase: InitPhase,
    pub secondary_phase: InitPhase,
    pub primary_base: u8,
    pub secondary_base: u8,
    pub primary_irr: u8,
    pub primary_isr: u8,
    pub secondary_irr: u8,
    pub secondary_isr: u8,
    pub primary_read_sel: ReadSel,
    pub secondary_read_sel: ReadSel,
    pub serial: Serial,
    pub pending_irq: u16,
    pit_channel0: PitChannel,
    pit_channel2: PitChannel,
    speaker_control: u8,
    rtc: RtcState,
    ps2: Ps2Controller,
}

impl Pic {
    pub fn new(tsc_khz: u64, guest_memory_size: u64) -> Self {
        Self {
            primary_mask: 0xFF,
            secondary_mask: 0xFF,
            primary_phase: InitPhase::Uninitialized,
            secondary_phase: InitPhase::Uninitialized,
            primary_base: 0,
            secondary_base: 0,
            primary_irr: 0,
            primary_isr: 0,
            secondary_irr: 0,
            secondary_isr: 0,
            primary_read_sel: ReadSel::Irr,
            secondary_read_sel: ReadSel::Irr,
            serial: Serial::default(),
            pending_irq: 0,
            pit_channel0: PitChannel::new(tsc_khz),
            pit_channel2: PitChannel::new(tsc_khz),
            speaker_control: 0,
            rtc: RtcState::new(guest_memory_size),
            ps2: Ps2Controller::new(),
        }
    }

    pub fn poll_timer(&mut self) {
        if self.pit_channel0.poll() {
            self.pending_irq |= 1;
        }
    }

    /// Returns whether another VM entry is needed to deliver a latched IRQ.
    ///
    /// In particular, a guest may handle the higher-priority PIT IRQ0 and
    /// immediately halt while UART IRQ4 is still pending. The scheduler must
    /// keep that VCPU in the current time slice long enough to inject IRQ4;
    /// otherwise multiple guests whose round-robin period exceeds the PIT
    /// period can starve serial input indefinitely.
    pub fn has_pending_interrupt(&self) -> bool {
        self.pending_irq != 0
    }

    fn write_pit_control(&mut self, value: u8) {
        match value >> 6 {
            0 => self.pit_channel0.write_control(value),
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

    pub fn handle_io(
        &mut self,
        regs: &mut GuestRegisters,
        qual: QualIo,
    ) -> Result<(), &'static str> {
        // String and REP I/O require guest-memory translation and partial
        // completion semantics. They are not implemented by this emulator, so
        // reject them without touching host I/O or guest memory.
        if qual.string() != 0 || qual.rep() != 0 {
            return Err("String/REP guest I/O is unsupported");
        }

        match qual.direction() {
            0 => {
                self.handle_io_out(regs, qual);
            }
            1 => {
                self.handle_io_in(regs, qual);
            }
            _ => {}
        }

        Ok(())
    }

    pub fn poll_serial_input(&mut self) {
        if self.serial.mcr & 0x10 == 0 {
            let mut input = [0; 16];
            let capacity = self.serial.remaining().min(input.len());
            let received = serial::poll_guest_input(&mut input[..capacity]);
            for &byte in &input[..received] {
                self.serial.enqueue(byte);
            }
        }
        if self.serial.mcr & 0x08 != 0 && self.serial.interrupt_pending() {
            self.pending_irq |= 1 << 4;
        }
    }

    pub fn inject_external_interrupt(&mut self) -> Result<bool, &'static str> {
        let pending = self.pending_irq;

        if pending == 0 {
            return Ok(false);
        }

        // Do not overwrite an exception or another event queued by the
        // previous VM-exit handler for the next VM entry.
        if vmread(vmx::vmcs::control::VMENTRY_INTERRUPTION_INFO_FIELD)? & (1 << 31) != 0 {
            return Ok(false);
        }

        if self.primary_phase != InitPhase::Initialized {
            return Ok(false);
        }

        let eflags = vmread(vmx::vmcs::guest::RFLAGS)?;
        if eflags >> 9 & 1 == 0 {
            return Ok(false);
        }

        let interruptibility = vmread(vmx::vmcs::guest::INTERRUPTIBILITY_STATE)?;
        if interruptibility & 0x3 != 0 {
            return Ok(false);
        }

        let is_secondary_masked = (self.primary_mask >> 2) & 1 != 0;

        for i in 0..16 {
            if is_secondary_masked && i >= 8 {
                continue;
            }

            let irq_bit = 1 << i;
            if pending & irq_bit == 0 {
                continue;
            }

            let delta = if i < 8 { i } else { i - 8 };
            let is_masked = if i < 8 {
                (self.primary_mask >> delta) & 1 != 0
            } else {
                let is_irq_masked = (self.secondary_mask >> delta) & 1 != 0;
                is_secondary_masked || is_irq_masked
            };

            if is_masked {
                continue;
            }

            let interrupt_info = EntryIntrInfo::new()
                .with_vector(
                    delta as u8
                        + if i < 8 {
                            self.primary_base
                        } else {
                            self.secondary_base
                        },
                )
                .with_typ(0)
                .with_ec_available(false)
                .with_valid(true);

            vmwrite(
                vmx::vmcs::control::VMENTRY_INTERRUPTION_INFO_FIELD,
                u32::from(interrupt_info) as u64,
            )?;

            self.pending_irq &= !irq_bit;
            return Ok(true);
        }

        Ok(false)
    }

    pub fn inject_exception(
        &mut self,
        vector: u32,
        error_code: Option<u32>,
    ) -> Result<(), &'static str> {
        let has_error_code = matches!(vector, 8 | 10..=14 | 17 | 21);

        let interrupt_info = EntryIntrInfo::new()
            .with_vector(vector as u8)
            .with_typ(3)
            .with_ec_available(has_error_code)
            .with_valid(true);

        vmwrite(
            vmx::vmcs::control::VMENTRY_INTERRUPTION_INFO_FIELD,
            u32::from(interrupt_info) as u64,
        )?;

        if has_error_code {
            let ec = error_code.unwrap_or(0);
            vmwrite(vmx::vmcs::control::VMENTRY_EXCEPTION_ERR_CODE, ec as u64)?;
        }

        Ok(())
    }

    fn handle_io_in(&mut self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.port() {
            // The outer kernel owns physical PCI and virtio I/O. Config-data
            // reads return the architectural "no device" value and physical
            // device BARs are never forwarded into the guest.
            0x0CF8..=0x0CFB => regs.rax = 0,
            0x0CFC..=0x0CFF => regs.rax = u32::MAX as u64,
            0xC000..=0xCFFF => regs.rax = u32::MAX as u64,
            0x20..=0x21 => self.handle_pic_in(regs, qual),
            0xA0..=0xA1 => self.handle_pic_in(regs, qual),
            0x0040 if qual.size() == 0 => regs.rax = self.pit_channel0.read() as u64,
            0x0042 if qual.size() == 0 => regs.rax = self.pit_channel2.read() as u64,
            0x0061 if qual.size() == 0 => regs.rax = self.speaker_status() as u64,
            0x0060 if qual.size() == 0 => regs.rax = self.ps2.read_data() as u64,
            0x0064 if qual.size() == 0 => regs.rax = self.ps2.status() as u64,
            0x0070 if qual.size() == 0 => regs.rax = self.rtc.selector as u64,
            0x0071 if qual.size() == 0 => regs.rax = self.rtc.read_data() as u64,
            0x03F8..=0x03FF => self.handle_serial_in(regs, qual),
            _ => regs.rax = 0,
        }
    }

    fn handle_io_out(&mut self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.port() {
            0x0CF8..=0x0CFF => {} //ignore
            0xC000..=0xCFFF => {} //ignore
            0x20..=0x21 => self.handle_pic_out(regs, qual),
            0xA0..=0xA1 => self.handle_pic_out(regs, qual),
            0x0040 if qual.size() == 0 => self.pit_channel0.write(regs.rax as u8),
            0x0042 if qual.size() == 0 => self.pit_channel2.write(regs.rax as u8),
            0x0043 if qual.size() == 0 => self.write_pit_control(regs.rax as u8),
            0x0061 if qual.size() == 0 => self.speaker_control = regs.rax as u8 & 0x03,
            0x0060 if qual.size() == 0 => self.ps2.write_data(regs.rax as u8),
            0x0064 if qual.size() == 0 => self.ps2.write_command(regs.rax as u8),
            0x0070 if qual.size() == 0 => self.rtc.selector = regs.rax as u8,
            0x0071 if qual.size() == 0 => self.rtc.write_data(regs.rax as u8),
            0x03F8..=0x03FF => self.handle_serial_out(regs, qual),
            _ => {}
        }
    }

    fn handle_serial_in(&mut self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.port() {
            0x3F8 if self.serial.lcr & 0x80 != 0 => regs.rax = self.serial.divisor_low as u64,
            0x3F8 => regs.rax = self.serial.dequeue() as u64,
            0x3F9 if self.serial.lcr & 0x80 != 0 => regs.rax = self.serial.divisor_high as u64,
            0x3F9 => regs.rax = self.serial.ier as u64,
            0x3FA => regs.rax = self.serial.interrupt_identification() as u64,
            0x3FB => regs.rax = self.serial.lcr as u64,
            0x3FC => regs.rax = self.serial.mcr as u64,
            0x3FD => {
                if qual.size() == 0 {
                    let mut status = 0x60;
                    if self.serial.rx_len != 0 {
                        status |= 1;
                    }
                    if self.serial.overrun {
                        status |= 2;
                        self.serial.overrun = false;
                    }
                    regs.rax = status;
                }
            }
            0x3FE => {
                if qual.size() == 0 {
                    regs.rax = 0xb0
                }
            }
            0x3FF => regs.rax = self.serial.scratch as u64,
            _ => regs.rax = 0,
        }
        if self.serial.mcr & 0x08 != 0 && self.serial.interrupt_pending() {
            self.pending_irq |= 1 << 4;
        }
    }

    fn handle_serial_out(&mut self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.port() {
            0x3F8 if self.serial.lcr & 0x80 != 0 => self.serial.divisor_low = regs.rax as u8,
            0x3F8 => {
                let byte = regs.rax as u8;
                if self.serial.mcr & 0x10 != 0 {
                    self.serial.enqueue(byte);
                } else {
                    serial::write_guest_raw_byte(byte);
                }
                // The emulated transmitter drains immediately. Its THRE edge
                // is acknowledged through IIR and raised again after every
                // subsequent THR write.
                self.serial.thre_interrupt_pending = self.serial.ier & 0x02 != 0;
            }
            0x3F9 if self.serial.lcr & 0x80 != 0 => self.serial.divisor_high = regs.rax as u8,
            0x3F9 => self.serial.write_ier(regs.rax as u8),
            0x3FA => self.serial.write_fcr(regs.rax as u8),
            0x3FB => self.serial.lcr = regs.rax as u8,
            0x3FC => self.serial.mcr = regs.rax as u8 & 0x1f,
            0x3FD => {}
            0x3FF => self.serial.scratch = regs.rax as u8,
            _ => {}
        }
        if self.serial.mcr & 0x08 != 0 && self.serial.interrupt_pending() {
            self.pending_irq |= 1 << 4;
        }
    }

    fn handle_pic_in(&self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.port() {
            0x20 => {
                let v = match self.primary_read_sel {
                    ReadSel::Irr => self.primary_irr,
                    ReadSel::Isr => self.primary_isr,
                };
                regs.rax = v as u64;
            }
            0xA0 => {
                let v = match self.secondary_read_sel {
                    ReadSel::Irr => self.secondary_irr,
                    ReadSel::Isr => self.secondary_isr,
                };
                regs.rax = v as u64;
            }
            0x21 => match self.primary_phase {
                InitPhase::Uninitialized | InitPhase::Initialized => {
                    regs.rax = self.primary_mask as u64;
                }
                _ => {}
            },
            0xA1 => match self.secondary_phase {
                InitPhase::Uninitialized | InitPhase::Initialized => {
                    regs.rax = self.secondary_mask as u64;
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn handle_pic_out(&mut self, regs: &mut GuestRegisters, qual: QualIo) {
        let pic = self;
        let dx = regs.rax as u8;
        match qual.port() {
            0x20 => match dx {
                0x11 => pic.primary_phase = InitPhase::Phase1,
                0x0A => pic.primary_read_sel = ReadSel::Isr,
                0x0B => pic.primary_read_sel = ReadSel::Irr,
                0x20 => {
                    pic.primary_isr = 0;
                }
                0x60..=0x67 => {
                    let irq = dx & 0x7;
                    pic.primary_isr &= !(1 << irq);
                }
                // Unsupported OCW/ICW commands are guest input. Ignore them
                // instead of allowing a malformed command to stop the host.
                _ => {}
            },
            0x21 => match pic.primary_phase {
                InitPhase::Uninitialized | InitPhase::Initialized => pic.primary_mask = dx,
                InitPhase::Phase1 => {
                    pic.primary_base = dx;
                    pic.primary_phase = InitPhase::Phase2;
                }
                InitPhase::Phase2 => {
                    pic.primary_phase = InitPhase::Phase3;
                }
                InitPhase::Phase3 => {
                    info!("Primary Pic Initialized");
                    pic.primary_phase = InitPhase::Initialized
                }
            },
            0xA0 => match dx {
                0x11 => pic.secondary_phase = InitPhase::Phase1,
                0x0A => pic.secondary_read_sel = ReadSel::Isr,
                0x0B => pic.secondary_read_sel = ReadSel::Irr,
                0x20 => {
                    pic.secondary_isr = 0;
                }
                0x60..=0x67 => {
                    let irq = dx & 0x7;
                    pic.secondary_isr &= !(1 << irq);
                }
                _ => {}
            },
            0xA1 => match pic.secondary_phase {
                InitPhase::Uninitialized | InitPhase::Initialized => pic.secondary_mask = dx,
                InitPhase::Phase1 => {
                    pic.secondary_base = dx;
                    pic.secondary_phase = InitPhase::Phase2;
                }
                InitPhase::Phase2 => {
                    pic.secondary_phase = InitPhase::Phase3;
                }
                InitPhase::Phase3 => {
                    info!("Secondary Pic Initialized");
                    pic.secondary_phase = InitPhase::Initialized
                }
            },
            _ => {}
        }
    }
}

pub struct IOBitmap {
    pub bitmap_a: PhysFrame,
    pub bitmap_b: PhysFrame,
}

impl IOBitmap {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Self {
        let bitmap_a = frame_allocator
            .allocate_frame()
            .expect("Failed to allocate I/O bitmap A");
        let bitmap_b = frame_allocator
            .allocate_frame()
            .expect("Failed to allocate I/O bitmap B");

        Self { bitmap_a, bitmap_b }
    }

    pub fn setup(&mut self) -> Result<(), &'static str> {
        let bitmap_a_addr = self.bitmap_a.start_address().as_u64() as usize;
        let bitmap_b_addr = self.bitmap_b.start_address().as_u64() as usize;

        unsafe {
            core::ptr::write_bytes(bitmap_a_addr as *mut u8, u8::MAX, 4096);
            core::ptr::write_bytes(bitmap_b_addr as *mut u8, u8::MAX, 4096);
        }

        vmwrite(vmcs::control::IO_BITMAP_A_ADDR_FULL, bitmap_a_addr as u64)?;
        vmwrite(vmcs::control::IO_BITMAP_B_ADDR_FULL, bitmap_b_addr as u64)?;

        Ok(())
    }
}
