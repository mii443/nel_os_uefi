//! Minimal architectural I/O APIC shared by the VMX and SVM backends.

const IO_APIC_INPUTS: usize = 24;
const IO_APIC_ID: u32 = 1;
const IO_APIC_VERSION: u32 = 0x11;
const REDIRECTION_BASE: u32 = 0x10;
const REDIRECTION_MASKED: u64 = 1 << 16;

pub struct IoApic {
    page_hpa: u64,
    redirection: [u64; IO_APIC_INPUTS],
    published_selector: u32,
    published_window: u32,
    line_levels: u32,
    pending: u32,
    in_service: u32,
}

impl IoApic {
    pub const fn new() -> Self {
        Self {
            page_hpa: 0,
            redirection: [REDIRECTION_MASKED; IO_APIC_INPUTS],
            published_selector: 0,
            published_window: 0,
            line_levels: 0,
            pending: 0,
            in_service: 0,
        }
    }

    pub fn attach_page(&mut self, hpa: u64) {
        self.page_hpa = hpa;
        self.redirection.fill(REDIRECTION_MASKED);
        self.line_levels = 0;
        self.pending = 0;
        self.in_service = 0;
        unsafe {
            core::ptr::write_bytes(hpa as *mut u8, 0, 4096);
            core::ptr::write_volatile((hpa + 0x10) as *mut u32, Self::identity_window());
        }
        self.published_selector = 0;
        self.published_window = Self::identity_window();
    }

    pub fn reset(&mut self) {
        let page_hpa = self.page_hpa;
        *self = Self::new();
        if page_hpa != 0 {
            self.attach_page(page_hpa);
        }
    }

    /// Capture the final selector/window transaction from the previous guest
    /// run and publish the selected register for the next run.
    pub fn synchronize(&mut self) {
        if self.page_hpa == 0 {
            return;
        }
        let selector = unsafe { core::ptr::read_volatile(self.page_hpa as *const u32) };
        let window = unsafe { core::ptr::read_volatile((self.page_hpa + 0x10) as *const u32) };
        if selector != self.published_selector || window != self.published_window {
            self.write_selected(selector, window);
        }
        // Keep the identity/version image available for a new selector written
        // and read back without an intervening VM exit. Redirection writes are
        // captured above before the window is restored.
        let published = Self::identity_window();
        unsafe {
            core::ptr::write_volatile((self.page_hpa + 0x10) as *mut u32, published);
        }
        self.published_selector = selector;
        self.published_window = published;
    }

    pub fn set_irq_level(&mut self, irq: u8, asserted: bool) {
        if usize::from(irq) >= IO_APIC_INPUTS {
            return;
        }
        let bit = 1u32 << irq;
        let was_asserted = self.line_levels & bit != 0;
        if asserted {
            self.line_levels |= bit;
            if !was_asserted {
                self.pending |= bit;
            }
        } else {
            self.line_levels &= !bit;
            self.pending &= !bit;
        }
    }

    pub fn pending_vector(&self) -> Option<(u8, u8)> {
        for irq in 0..IO_APIC_INPUTS {
            if self.pending & (1 << irq) == 0 {
                continue;
            }
            let low = self.redirection[irq] as u32;
            let vector = low as u8;
            if low & REDIRECTION_MASKED as u32 == 0 && vector >= 16 {
                return Some((irq as u8, vector));
            }
        }
        None
    }

    pub fn acknowledge(&mut self, irq: u8) {
        if usize::from(irq) < IO_APIC_INPUTS {
            let bit = 1u32 << irq;
            self.pending &= !bit;
            if self.redirection[usize::from(irq)] & (1 << 15) == 0 {
                // Edge-triggered inputs consume the latched transition.
                self.line_levels &= !bit;
            } else {
                self.in_service |= bit;
            }
        }
    }

    /// Complete level-triggered deliveries after a local APIC EOI. An input
    /// that is still asserted becomes pending again, matching remote-IRR
    /// semantics without creating an interrupt storm before the guest EOI.
    pub fn eoi(&mut self) {
        let retrigger = self.in_service & self.line_levels;
        self.in_service = 0;
        self.pending |= retrigger;
    }

    pub fn has_pending_interrupt(&self) -> bool {
        self.pending_vector().is_some()
    }

    pub fn is_level_triggered(&self, irq: u8) -> bool {
        usize::from(irq) < IO_APIC_INPUTS && self.redirection[usize::from(irq)] & (1 << 15) != 0
    }

    fn identity_window() -> u32 {
        (IO_APIC_ID << 24) | ((IO_APIC_INPUTS as u32 - 1) << 16) | IO_APIC_VERSION
    }

    fn write_selected(&mut self, selector: u32, value: u32) {
        if !(REDIRECTION_BASE..=0x3f).contains(&selector) {
            return;
        }
        let offset = selector - REDIRECTION_BASE;
        let entry = &mut self.redirection[offset as usize / 2];
        if offset & 1 == 0 {
            *entry = (*entry & 0xffff_ffff_0000_0000) | u64::from(value);
        } else {
            *entry = (*entry & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
        }
    }
}

impl Default for IoApic {
    fn default() -> Self {
        Self::new()
    }
}
