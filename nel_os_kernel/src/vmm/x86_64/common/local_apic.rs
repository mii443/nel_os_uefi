//! Architectural local APIC state shared by the VMX and SVM backends.
//!
//! This exposes the standard x2APIC MSR interface for a uniprocessor guest.
//! Legacy device interrupts continue to enter through the PC-AT PIC path.

const APIC_VERSION: u32 = 0x0005_0014;
const LVT_MASKED: u32 = 1 << 16;
const XAPIC_EOI_OFFSET: u64 = 0xb0;
const XAPIC_EOI_SENTINEL: u32 = u32::MAX;

pub struct LocalApic {
    page_hpa: u64,
    tpr: u32,
    svr: u32,
    esr: u32,
    icr: u64,
    lvt_cmci: u32,
    lvt_timer: u32,
    lvt_thermal: u32,
    lvt_pmi: u32,
    lvt_lint0: u32,
    lvt_lint1: u32,
    lvt_error: u32,
    initial_count: u32,
    divide_configuration: u32,
    timer_start_tsc: u64,
    timer_armed: bool,
    timer_pending: bool,
    in_service: [u32; 8],
    level_triggered: [u32; 8],
}

impl LocalApic {
    pub const fn new() -> Self {
        Self {
            page_hpa: 0,
            tpr: 0,
            svr: 0xff,
            esr: 0,
            icr: 0,
            lvt_cmci: LVT_MASKED,
            lvt_timer: LVT_MASKED,
            lvt_thermal: LVT_MASKED,
            lvt_pmi: LVT_MASKED,
            lvt_lint0: LVT_MASKED,
            lvt_lint1: LVT_MASKED,
            lvt_error: LVT_MASKED,
            initial_count: 0,
            divide_configuration: 0,
            timer_start_tsc: 0,
            timer_armed: false,
            timer_pending: false,
            in_service: [0; 8],
            level_triggered: [0; 8],
        }
    }

    pub fn reset(&mut self) {
        let page_hpa = self.page_hpa;
        *self = Self::new();
        if page_hpa != 0 {
            self.attach_page(page_hpa);
        }
    }

    pub fn attach_page(&mut self, hpa: u64) {
        self.page_hpa = hpa;
        unsafe {
            core::ptr::write_volatile((hpa + 0x20) as *mut u32, 0);
            core::ptr::write_volatile((hpa + 0x30) as *mut u32, APIC_VERSION);
            core::ptr::write_volatile((hpa + XAPIC_EOI_OFFSET) as *mut u32, XAPIC_EOI_SENTINEL);
        }
    }

    /// Observe an xAPIC MMIO EOI write on the shared register page. x2APIC
    /// EOIs are intercepted as WRMSR by each virtualization backend instead.
    pub fn take_mmio_eoi(&mut self) -> Option<bool> {
        if self.page_hpa == 0 {
            return None;
        }
        let address = self.page_hpa + XAPIC_EOI_OFFSET;
        let value = unsafe { core::ptr::read_volatile(address as *const u32) };
        if value != 0 {
            return None;
        }
        unsafe {
            core::ptr::write_volatile(address as *mut u32, XAPIC_EOI_SENTINEL);
        }
        Some(self.eoi())
    }

    pub fn read_x2apic(&self, index: u32) -> Option<u64> {
        Some(match index {
            0x802 => 0,
            0x803 => u64::from(APIC_VERSION),
            0x808 => u64::from(self.tpr),
            0x80a => u64::from(self.tpr & 0xf0),
            0x80d => 1,
            0x80f => u64::from(self.svr),
            0x810..=0x817 => u64::from(self.in_service[(index - 0x810) as usize]),
            0x818..=0x81f => u64::from(self.level_triggered[(index - 0x818) as usize]),
            0x820..=0x827 => 0,
            0x828 => u64::from(self.esr),
            0x82f => u64::from(self.lvt_cmci),
            0x830 => self.icr,
            0x832 => u64::from(self.lvt_timer),
            0x833 => u64::from(self.lvt_thermal),
            0x834 => u64::from(self.lvt_pmi),
            0x835 => u64::from(self.lvt_lint0),
            0x836 => u64::from(self.lvt_lint1),
            0x837 => u64::from(self.lvt_error),
            0x838 => u64::from(self.initial_count),
            0x839 => u64::from(self.current_count()),
            0x83e => u64::from(self.divide_configuration),
            _ => return None,
        })
    }

    pub fn write_x2apic(&mut self, index: u32, value: u64) -> Option<()> {
        match index {
            0x808 => self.tpr = value as u32 & 0xff,
            0x80b => {}
            0x80f => self.svr = value as u32 & 0x3ff,
            0x828 => self.esr = 0,
            0x82f => self.lvt_cmci = value as u32,
            0x830 => self.icr = value,
            0x832 => self.lvt_timer = value as u32,
            0x833 => self.lvt_thermal = value as u32,
            0x834 => self.lvt_pmi = value as u32,
            0x835 => self.lvt_lint0 = value as u32,
            0x836 => self.lvt_lint1 = value as u32,
            0x837 => self.lvt_error = value as u32,
            0x838 => {
                self.initial_count = value as u32;
                self.timer_start_tsc = Self::now();
                self.timer_armed = self.initial_count != 0;
                self.timer_pending = false;
            }
            0x83e => self.divide_configuration = value as u32 & 0xb,
            0x83f => self.icr = (value & 0xff) | (1 << 18),
            _ => return None,
        }
        Some(())
    }

    /// Update the architectural timer and return a deliverable vector.
    pub fn pending_timer_vector(&mut self) -> Option<u8> {
        if self.timer_armed && self.timer_expired() {
            let periodic = self.lvt_timer & (1 << 17) != 0;
            if periodic {
                let period = u64::from(self.initial_count) * self.timer_divisor();
                let elapsed = Self::now().wrapping_sub(self.timer_start_tsc);
                let periods = (elapsed / period).max(1);
                self.timer_start_tsc = self
                    .timer_start_tsc
                    .wrapping_add(period.wrapping_mul(periods));
            } else {
                self.timer_armed = false;
            }

            if self.lvt_timer & LVT_MASKED == 0 && self.svr & (1 << 8) != 0 {
                self.timer_pending = true;
            }
        }

        self.timer_pending
            .then_some((self.lvt_timer & 0xff) as u8)
            .filter(|vector| *vector >= 16)
    }

    pub fn acknowledge_timer(&mut self) {
        self.timer_pending = false;
    }

    pub fn accept_interrupt(&mut self, vector: u8, level: bool) {
        let bank = usize::from(vector / 32);
        let bit = 1u32 << (vector % 32);
        self.in_service[bank] |= bit;
        if level {
            self.level_triggered[bank] |= bit;
        } else {
            self.level_triggered[bank] &= !bit;
        }
        self.sync_interrupt_bank(bank);
    }

    /// Clear the highest-priority in-service vector and report whether it was
    /// delivered from a level-triggered source.
    pub fn eoi(&mut self) -> bool {
        for bank in (0..self.in_service.len()).rev() {
            let entries = self.in_service[bank];
            if entries == 0 {
                continue;
            }
            let bit_index = 31 - entries.leading_zeros();
            let bit = 1u32 << bit_index;
            self.in_service[bank] &= !bit;
            let level = self.level_triggered[bank] & bit != 0;
            self.level_triggered[bank] &= !bit;
            self.sync_interrupt_bank(bank);
            return level;
        }
        false
    }

    pub fn has_pending_timer(&self) -> bool {
        self.timer_pending
    }

    fn current_count(&self) -> u32 {
        if !self.timer_armed || self.initial_count == 0 {
            return 0;
        }
        let elapsed = Self::now().wrapping_sub(self.timer_start_tsc) / self.timer_divisor();
        if self.lvt_timer & (1 << 17) != 0 {
            let position = elapsed % u64::from(self.initial_count);
            self.initial_count.saturating_sub(position as u32)
        } else {
            self.initial_count
                .saturating_sub(elapsed.min(u64::from(u32::MAX)) as u32)
        }
    }

    fn timer_expired(&self) -> bool {
        Self::now().wrapping_sub(self.timer_start_tsc)
            >= u64::from(self.initial_count) * self.timer_divisor()
    }

    fn timer_divisor(&self) -> u64 {
        match self.divide_configuration & 0xb {
            0x0 => 2,
            0x1 => 4,
            0x2 => 8,
            0x3 => 16,
            0x8 => 32,
            0x9 => 64,
            0xa => 128,
            0xb => 1,
            _ => 2,
        }
    }

    fn sync_interrupt_bank(&self, bank: usize) {
        if self.page_hpa == 0 {
            return;
        }
        unsafe {
            core::ptr::write_volatile(
                (self.page_hpa + 0x100 + bank as u64 * 0x10) as *mut u32,
                self.in_service[bank],
            );
            core::ptr::write_volatile(
                (self.page_hpa + 0x180 + bank as u64 * 0x10) as *mut u32,
                self.level_triggered[bank],
            );
        }
    }

    fn now() -> u64 {
        unsafe { x86::time::rdtsc() }
    }
}

impl Default for LocalApic {
    fn default() -> Self {
        Self::new()
    }
}
