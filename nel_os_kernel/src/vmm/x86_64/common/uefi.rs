pub const FIRMWARE_BASE: u64 = 0xffe0_0000;
pub const FIRMWARE_SIZE: usize = 2 * 1024 * 1024;
pub const RESET_VECTOR_IP: u64 = 0xfff0;
pub const RESET_VECTOR_CS: u16 = 0xf000;
pub const RESET_VECTOR_CS_BASE: u64 = 0xffff_0000;
// Keep the 32-bit PCI/firmware aperture free. RAM beyond this limit is
// exposed above 4 GiB, matching the conventional QEMU PC memory layout that
// OVMF understands through the CMOS low/high-memory fields.
pub const LOW_MEMORY_LIMIT: u64 = 0x8000_0000;
pub const HIGH_MEMORY_BASE: u64 = 0x1_0000_0000;
pub const PLATFORM_MMIO_PAGES: [u64; 5] = [
    0x8000_0000,
    0xfec0_0000,
    0xfed0_0000,
    0xfed4_0000,
    0xfee0_0000,
];

pub fn ram_ranges(memory_size: u64) -> [(u64, u64); 2] {
    let low_size = memory_size.min(LOW_MEMORY_LIMIT);
    [
        (0, low_size),
        (HIGH_MEMORY_BASE, memory_size.saturating_sub(low_size)),
    ]
}

pub fn low_memory_size(memory_size: u64) -> u64 {
    memory_size.min(LOW_MEMORY_LIMIT)
}

pub fn high_memory_size(memory_size: u64) -> u64 {
    memory_size.saturating_sub(LOW_MEMORY_LIMIT)
}

pub fn ram_range_containing(memory_size: u64, address: u64) -> Option<(u64, u64)> {
    ram_ranges(memory_size)
        .into_iter()
        .find(|(base, size)| *size != 0 && address >= *base && address < base.saturating_add(*size))
}

pub fn firmware_image() -> Result<&'static [u8], &'static str> {
    let address = *crate::GUEST_FIRMWARE_ADDR
        .get()
        .ok_or("Guest UEFI firmware address is unavailable")?;
    let size = *crate::GUEST_FIRMWARE_SIZE
        .get()
        .ok_or("Guest UEFI firmware size is unavailable")?;
    if address == 0 || size != FIRMWARE_SIZE as u64 {
        return Err("Guest UEFI firmware must be exactly 2 MiB");
    }
    Ok(unsafe { core::slice::from_raw_parts(address as *const u8, FIRMWARE_SIZE) })
}
