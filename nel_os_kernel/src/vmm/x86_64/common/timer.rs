//! Standard virtual platform clocks.

/// ACPI PM timer clock frequency mandated by the ACPI specification.
const ACPI_PM_HZ: u128 = 3_579_545;

pub struct AcpiPmTimer {
    start_tsc: u64,
}

impl AcpiPmTimer {
    pub fn new() -> Self {
        Self {
            start_tsc: Self::now(),
        }
    }

    pub fn reset(&mut self) {
        self.start_tsc = Self::now();
    }

    pub fn read(&self) -> u32 {
        let tsc_khz = crate::interrupt::apic::GUEST_TSC_KHZ
            .get()
            .copied()
            .unwrap_or(1_000_000);
        let elapsed = Self::now().wrapping_sub(self.start_tsc);
        ((u128::from(elapsed) * ACPI_PM_HZ / (u128::from(tsc_khz) * 1_000)) & u128::from(u32::MAX))
            as u32
    }

    fn now() -> u64 {
        unsafe { x86::time::rdtsc() }
    }
}

impl Default for AcpiPmTimer {
    fn default() -> Self {
        Self::new()
    }
}

/// ACPI PM1 fixed-event and control registers for the virtual PIIX4 device.
///
/// Event status bits remain clear until a virtual fixed event is implemented,
/// while enable and control writes are retained so ACPICA can verify the
/// standard register interface.  SCI_EN is always set because firmware hands
/// the platform to the guest in ACPI mode.
pub struct AcpiPmRegisters {
    event_enable: u16,
    control: u16,
}

impl AcpiPmRegisters {
    pub const fn new() -> Self {
        Self {
            event_enable: 0,
            control: 1,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn read(&self, port: u16, size: u8) -> Option<u32> {
        match (port, size) {
            (0xb000, 2) => Some(0),
            (0xb000, 4) => Some(u32::from(self.event_enable) << 16),
            (0xb002, 2) => Some(u32::from(self.event_enable)),
            (0xb004, 2) => Some(u32::from(self.control)),
            _ => None,
        }
    }

    pub fn write(&mut self, port: u16, size: u8, value: u32) -> bool {
        match (port, size) {
            // PM1 status is write-one-to-clear. No status is asserted yet.
            (0xb000, 2) => {}
            (0xb000, 4) => self.event_enable = (value >> 16) as u16,
            (0xb002, 2) => self.event_enable = value as u16,
            (0xb004, 2) => self.control = value as u16 | 1,
            _ => return false,
        }
        true
    }
}

impl Default for AcpiPmRegisters {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::AcpiPmRegisters;

    #[test]
    fn pm1_enable_and_control_registers_read_back_writes() {
        let mut registers = AcpiPmRegisters::new();

        assert!(registers.write(0xb002, 2, 0x1320));
        assert_eq!(registers.read(0xb000, 4), Some(0x1320_0000));
        assert_eq!(registers.read(0xb002, 2), Some(0x1320));
        assert!(registers.write(0xb004, 2, 0));
        assert_eq!(registers.read(0xb004, 2), Some(1));
    }
}
