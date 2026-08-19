//! ACPI tables exposed to guest firmware through the standard QEMU fw_cfg
//! table-loader interface.
//!
//! The described machine matches the devices the hypervisor actually
//! emulates: one processor, an i440FX/PIIX PCI/ISA platform, local and I/O
//! APICs, legacy 8259 interrupts and timers, RTC, 8042, COM1, and an ACPI PM
//! timer. OVMF relocates the blob, installs its platform tables, and publishes
//! the guest RSDP and root tables.

pub const TABLES_SELECTOR: u16 = 0x20;
pub const LOADER_SELECTOR: u16 = 0x21;

const TABLES_FILE: &[u8] = b"etc/acpi/tables";
const LOADER_FILE: &[u8] = b"etc/table-loader";
const DSDT: &[u8] = include_bytes!("acpi/dsdt.aml");

const ACPI_HEADER_SIZE: usize = 36;
const XSDT_OFFSET: usize = 0;
const XSDT_SIZE: usize = ACPI_HEADER_SIZE + 16;
const FADT_OFFSET: usize = 64;
const FADT_SIZE: usize = 244;
const FACS_OFFSET: usize = 320;
const FACS_SIZE: usize = 64;
const DSDT_OFFSET: usize = FACS_OFFSET + FACS_SIZE;
const MADT_OFFSET: usize = (DSDT_OFFSET + DSDT.len() + 7) & !7;
const MADT_SIZE: usize = ACPI_HEADER_SIZE + 8 + 8 + 12;
pub const TABLES_SIZE: usize = MADT_OFFSET + MADT_SIZE;

const LOADER_ENTRY_SIZE: usize = 128;
const LOADER_ENTRY_COUNT: usize = 10;
pub const LOADER_SIZE: usize = LOADER_ENTRY_COUNT * LOADER_ENTRY_SIZE;

pub const DIRECTORY_SIZE: usize = 4 + 2 * 64;

pub fn directory_byte(offset: usize) -> Option<u8> {
    if offset < 4 {
        return Some(2u32.to_be_bytes()[offset]);
    }

    let entry = (offset - 4) / 64;
    let field = (offset - 4) % 64;
    let (size, selector, name) = match entry {
        0 => (TABLES_SIZE as u32, TABLES_SELECTOR, TABLES_FILE),
        1 => (LOADER_SIZE as u32, LOADER_SELECTOR, LOADER_FILE),
        _ => return None,
    };

    match field {
        0..=3 => Some(size.to_be_bytes()[field]),
        4..=5 => Some(selector.to_be_bytes()[field - 4]),
        6..=7 => Some(0),
        8..=63 => Some(name.get(field - 8).copied().unwrap_or(0)),
        _ => None,
    }
}

pub fn file_byte(selector: u16, offset: usize) -> Option<u8> {
    match selector {
        TABLES_SELECTOR => tables_byte(offset),
        LOADER_SELECTOR => loader_byte(offset),
        _ => None,
    }
}

fn tables_byte(offset: usize) -> Option<u8> {
    if offset >= TABLES_SIZE {
        return None;
    }
    if offset < XSDT_OFFSET + XSDT_SIZE {
        return Some(xsdt_byte(offset - XSDT_OFFSET));
    }
    if (FADT_OFFSET..FADT_OFFSET + FADT_SIZE).contains(&offset) {
        return Some(fadt_byte(offset - FADT_OFFSET));
    }
    if (FACS_OFFSET..FACS_OFFSET + FACS_SIZE).contains(&offset) {
        return Some(facs_byte(offset - FACS_OFFSET));
    }
    if (DSDT_OFFSET..DSDT_OFFSET + DSDT.len()).contains(&offset) {
        return DSDT.get(offset - DSDT_OFFSET).copied();
    }
    if offset >= MADT_OFFSET {
        return Some(madt_byte(offset - MADT_OFFSET));
    }
    Some(0)
}

fn xsdt_byte(offset: usize) -> u8 {
    if offset < ACPI_HEADER_SIZE {
        return header_byte(b"XSDT", XSDT_SIZE as u32, 1, offset);
    }
    match offset {
        36..=43 => byte_from_u64(FADT_OFFSET as u64, offset - 36),
        44..=51 => byte_from_u64(MADT_OFFSET as u64, offset - 44),
        _ => 0,
    }
}

fn fadt_byte(offset: usize) -> u8 {
    if offset < ACPI_HEADER_SIZE {
        return header_byte(b"FACP", FADT_SIZE as u32, 3, offset);
    }

    match offset {
        // Relative pointers are relocated by OVMF's fw_cfg table loader.
        36..=39 => byte_from_u32(FACS_OFFSET as u32, offset - 36),
        40..=43 => byte_from_u32(DSDT_OFFSET as u32, offset - 40),
        45 => 1,                                  // Desktop preferred power profile.
        46..=47 => byte_from_u16(9, offset - 46), // Conventional SCI IRQ.
        56..=59 => byte_from_u32(0xb000, offset - 56),
        64..=67 => byte_from_u32(0xb004, offset - 64),
        76..=79 => byte_from_u32(0xb008, offset - 76),
        88 => 4,     // PM1 event block: 16-bit status + 16-bit enable.
        89 => 2,     // PM1 control register length.
        91 => 4,     // PM timer register length.
        108 => 0x32, // RTC century CMOS index.
        // Legacy devices + 8042, but no VGA-compatible display hardware.
        109..=110 => byte_from_u16(0x0007, offset - 109),
        112..=115 => byte_from_u32(0x0105, offset - 112), // WBINVD, C1, 32-bit timer.
        131 => 0,
        132..=139 => byte_from_u64(FACS_OFFSET as u64, offset - 132),
        140..=147 => byte_from_u64(DSDT_OFFSET as u64, offset - 140),
        // Extended PM1a event GAS: System I/O, 32 bits, word access, 0xb000.
        148 => 1,
        149 => 32,
        151 => 2,
        152..=159 => byte_from_u64(0xb000, offset - 152),
        // Extended PM1a control GAS: System I/O, 16 bits, word access, 0xb004.
        172 => 1,
        173 => 16,
        175 => 2,
        176..=183 => byte_from_u64(0xb004, offset - 176),
        // Extended PM timer GAS: System I/O, 32 bits, dword access, 0xb008.
        208 => 1,
        209 => 32,
        211 => 3,
        212..=219 => byte_from_u64(0xb008, offset - 212),
        _ => 0,
    }
}

fn facs_byte(offset: usize) -> u8 {
    match offset {
        0..=3 => b"FACS"[offset],
        4..=7 => byte_from_u32(FACS_SIZE as u32, offset - 4),
        32 => 2, // ACPI 4.0+ FACS version.
        _ => 0,
    }
}

fn madt_byte(offset: usize) -> u8 {
    if offset < ACPI_HEADER_SIZE {
        return header_byte(b"APIC", MADT_SIZE as u32, 5, offset);
    }
    match offset {
        36..=39 => byte_from_u32(0xfee0_0000, offset - 36),
        40..=43 => byte_from_u32(1, offset - 40),
        44 => 0,
        45 => 8,
        46 => 0,
        47 => 0,
        48..=51 => byte_from_u32(1, offset - 48),
        // One standard 24-input I/O APIC at the architectural PC address.
        52 => 1,
        53 => 12,
        54 => 1,
        56..=59 => byte_from_u32(0xfec0_0000, offset - 56),
        60..=63 => byte_from_u32(0, offset - 60),
        _ => 0,
    }
}

fn header_byte(signature: &[u8; 4], length: u32, revision: u8, offset: usize) -> u8 {
    match offset {
        0..=3 => signature[offset],
        4..=7 => byte_from_u32(length, offset - 4),
        8 => revision,
        9 => 0, // Patched after address relocation by table-loader.
        10..=15 => b"NEL   "[offset - 10],
        16..=23 => b"NELPC   "[offset - 16],
        24..=27 => byte_from_u32(1, offset - 24),
        28..=31 => b"NEL "[offset - 28],
        32..=35 => byte_from_u32(1, offset - 32),
        _ => 0,
    }
}

fn loader_byte(offset: usize) -> Option<u8> {
    if offset >= LOADER_SIZE {
        return None;
    }
    let entry = offset / LOADER_ENTRY_SIZE;
    let field = offset % LOADER_ENTRY_SIZE;

    Some(match entry {
        0 => allocate_command_byte(field),
        1 => add_pointer_command_byte(field, XSDT_OFFSET + 36, 8),
        2 => add_pointer_command_byte(field, XSDT_OFFSET + 44, 8),
        3 => add_pointer_command_byte(field, FADT_OFFSET + 36, 4),
        4 => add_pointer_command_byte(field, FADT_OFFSET + 40, 4),
        5 => add_pointer_command_byte(field, FADT_OFFSET + 132, 8),
        6 => add_pointer_command_byte(field, FADT_OFFSET + 140, 8),
        7 => add_checksum_command_byte(field, XSDT_OFFSET, XSDT_SIZE),
        8 => add_checksum_command_byte(field, FADT_OFFSET, FADT_SIZE),
        9 => add_checksum_command_byte(field, MADT_OFFSET, MADT_SIZE),
        _ => 0,
    })
}

fn allocate_command_byte(field: usize) -> u8 {
    match field {
        0..=3 => byte_from_u32(1, field),
        4..=59 => TABLES_FILE.get(field - 4).copied().unwrap_or(0),
        60..=63 => byte_from_u32(64, field - 60),
        64 => 1, // QemuLoaderAllocHigh.
        _ => 0,
    }
}

fn add_pointer_command_byte(field: usize, pointer_offset: usize, pointer_size: u8) -> u8 {
    match field {
        0..=3 => byte_from_u32(2, field),
        4..=59 => TABLES_FILE.get(field - 4).copied().unwrap_or(0),
        60..=115 => TABLES_FILE.get(field - 60).copied().unwrap_or(0),
        116..=119 => byte_from_u32(pointer_offset as u32, field - 116),
        120 => pointer_size,
        _ => 0,
    }
}

fn add_checksum_command_byte(field: usize, start: usize, length: usize) -> u8 {
    match field {
        0..=3 => byte_from_u32(3, field),
        4..=59 => TABLES_FILE.get(field - 4).copied().unwrap_or(0),
        60..=63 => byte_from_u32((start + 9) as u32, field - 60),
        64..=67 => byte_from_u32(start as u32, field - 64),
        68..=71 => byte_from_u32(length as u32, field - 68),
        _ => 0,
    }
}

fn byte_from_u16(value: u16, offset: usize) -> u8 {
    value.to_le_bytes().get(offset).copied().unwrap_or(0)
}

fn byte_from_u32(value: u32, offset: usize) -> u8 {
    value.to_le_bytes().get(offset).copied().unwrap_or(0)
}

fn byte_from_u64(value: u64, offset: usize) -> u8 {
    value.to_le_bytes().get(offset).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_describes_both_acpi_files() {
        assert_eq!(
            (0..4)
                .map(|i| directory_byte(i).unwrap())
                .collect::<Vec<_>>(),
            [0, 0, 0, 2]
        );
        assert_eq!(directory_byte(DIRECTORY_SIZE), None);
    }

    #[test]
    fn dsdt_is_valid_and_blob_pointers_are_relative() {
        assert_eq!(
            DSDT.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            0
        );
        let pointer = (0..8)
            .map(|i| (tables_byte(ACPI_HEADER_SIZE + i).unwrap() as u64) << (8 * i))
            .fold(0, |value, byte| value | byte);
        assert_eq!(pointer, FADT_OFFSET as u64);
    }
}
