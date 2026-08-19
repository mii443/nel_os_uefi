pub struct FwCfg {
    selector: u16,
    offset: usize,
}

impl FwCfg {
    pub const fn new() -> Self {
        Self {
            selector: u16::MAX,
            offset: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn select(&mut self, selector: u16) {
        self.selector = selector;
        self.offset = 0;
    }

    pub fn read_u8(&mut self, guest_memory_size: u64) -> u8 {
        let byte = match self.selector {
            0x00 => b"QEMU".get(self.offset).copied(),
            0x01 => 1u32.to_le_bytes().get(self.offset).copied(),
            0x03 => guest_memory_size.to_le_bytes().get(self.offset).copied(),
            0x04 => 1u16.to_le_bytes().get(self.offset).copied(),
            0x05 | 0x0f => 1u16.to_le_bytes().get(self.offset).copied(),
            // An empty fw_cfg file directory has a big-endian zero count.
            0x19 => [0u8; 4].get(self.offset).copied(),
            _ => None,
        }
        .unwrap_or(0);
        self.offset = self.offset.saturating_add(1);
        byte
    }
}

impl Default for FwCfg {
    fn default() -> Self {
        Self::new()
    }
}
