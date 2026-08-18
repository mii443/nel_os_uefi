use core::{
    fmt::{self, Write},
    ptr::read_unaligned,
};

use crate::{BZIMAGE_ADDR, BZIMAGE_SIZE, info, vmm::VCpu};

struct StackText<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> StackText<N> {
    const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl<const N: usize> fmt::Write for StackText<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(fmt::Error)?;
        let destination = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        destination.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

pub fn load_kernel(vcpu: &mut dyn VCpu) -> Result<(), &'static str> {
    info!("Loading kernel into guest memory");
    let kernel_addr = BZIMAGE_ADDR.get().unwrap();
    let kernel_size = BZIMAGE_SIZE.get().unwrap();

    let kernel =
        unsafe { core::slice::from_raw_parts(*kernel_addr as *const u8, *kernel_size as usize) };

    let initrd_addr = crate::ROOTFS_ADDR.get().unwrap();
    let initrd_size = crate::ROOTFS_SIZE.get().unwrap();

    let initrd =
        unsafe { core::slice::from_raw_parts(*initrd_addr as *const u8, *initrd_size as usize) };

    info!("Creating boot parameters");
    let guest_mem_size = vcpu.get_guest_memory_size();
    if guest_mem_size <= LAYOUT_KERNEL_BASE {
        return Err("Guest memory is too small for the Linux kernel load address");
    }
    let mut bp = BootParams::from_bytes(kernel)?;
    bp.e820_entries = 0;

    let code_offset = bp.hdr.get_protected_code_offset();
    let protected_kernel = kernel
        .get(code_offset..)
        .ok_or("Linux protected-mode kernel offset is outside the image")?;
    let kernel_image_end = LAYOUT_KERNEL_BASE
        .checked_add(protected_kernel.len() as u64)
        .ok_or("Linux kernel load range overflowed")?;
    let preferred_address = bp.hdr.pref_address;
    let runtime_base = if (LAYOUT_KERNEL_BASE..guest_mem_size).contains(&preferred_address) {
        preferred_address
    } else {
        LAYOUT_KERNEL_BASE
    };
    let kernel_runtime_end = runtime_base
        .checked_add(bp.hdr.init_size as u64)
        .ok_or("Linux kernel runtime range overflowed")?;
    let occupied_end = kernel_image_end.max(kernel_runtime_end);
    let initrd_size = u64::try_from(initrd.len()).map_err(|_| "Initrd size does not fit in u64")?;
    let initrd_address = choose_initrd_address(
        guest_mem_size,
        bp.hdr.initrd_addr_max,
        initrd_size,
        occupied_end,
    )?;

    bp.hdr.type_of_loader = 0xFF;
    bp.hdr.ext_loader_ver = 0;
    bp.hdr.loadflags.set_loaded_high(true);
    bp.hdr.loadflags.set_can_use_heap(true);
    bp.hdr.heap_end_ptr = (LAYOUT_BOOTPARAM - 0x200) as u16;
    bp.hdr.loadflags.set_keep_segments(true);
    bp.hdr.cmd_line_ptr = LAYOUT_CMDLINE as u32;
    bp.hdr.vid_mode = 0xFFFF;
    bp.hdr.ramdisk_image = u32::try_from(initrd_address)
        .map_err(|_| "Initrd load address does not fit in Linux boot parameters")?;
    bp.hdr.ramdisk_size = u32::try_from(initrd.len())
        .map_err(|_| "Initrd size does not fit in Linux boot parameters")?;

    bp.add_e820_entry(0, LAYOUT_KERNEL_BASE, E820Type::Ram);
    bp.add_e820_entry(
        LAYOUT_KERNEL_BASE,
        guest_mem_size - LAYOUT_KERNEL_BASE,
        E820Type::Ram,
    );

    info!("Creating command line");
    let cmdline_max_size = if bp.hdr.cmdline_size < 256 {
        bp.hdr.cmdline_size
    } else {
        256
    };

    let cmdline_start = LAYOUT_CMDLINE;
    let cmdline_end = cmdline_start + cmdline_max_size as u64;
    vcpu.write_memory_ranged(cmdline_start, cmdline_end, 0)?;
    let tsc_khz = crate::interrupt::apic::GUEST_TSC_KHZ
        .get()
        .copied()
        .ok_or("TSC frequency was not calibrated before guest setup")?;
    // PCI devices belong to the outer nel_os instance. Passing their MMIO and
    // DMA through to an NPT-backed L2 guest would bypass guest-RAM translation.
    // The guest observes the same TSC as this single-vCPU host (no SVM TSC
    // offset/scaling), so the ACPI PM-timer measurement is its exact early
    // calibration reference as well.
    let mut cmdline = StackText::<256>::new();
    write!(
        cmdline,
        "console=ttyS0 earlyprintk=serial nokaslr pci=off tsc_early_khz={} tsc=reliable",
        tsc_khz
    )
    .map_err(|_| "Linux command line exceeds its stack buffer")?;
    let cmdline_bytes = cmdline.as_bytes();
    if cmdline_bytes.len() >= cmdline_max_size as usize {
        return Err("Linux command line is too small for the measured TSC frequency");
    }
    for (i, &byte) in cmdline_bytes.iter().enumerate() {
        vcpu.write_memory(cmdline_start + i as u64, byte)?;
    }

    info!("Loading boot parameters into guest memory");
    let bp_bytes = unsafe {
        core::slice::from_raw_parts(
            &bp as *const BootParams as *const u8,
            core::mem::size_of::<BootParams>(),
        )
    };
    load_image(vcpu, bp_bytes, LAYOUT_BOOTPARAM as usize)?;

    info!("Loading kernel image into guest memory");
    load_image(vcpu, protected_kernel, LAYOUT_KERNEL_BASE as usize)?;

    info!("Loading initrd image into guest memory");
    load_image(vcpu, initrd, initrd_address as usize)?;

    Ok(())
}

fn choose_initrd_address(
    guest_mem_size: u64,
    initrd_addr_max: u32,
    initrd_size: u64,
    occupied_end: u64,
) -> Result<u64, &'static str> {
    const PAGE_SIZE: u64 = 4096;
    const BOOT_PARAM_ADDRESS_LIMIT: u64 = u32::MAX as u64 + 1;

    let protocol_limit = if initrd_addr_max == 0 {
        BOOT_PARAM_ADDRESS_LIMIT
    } else {
        initrd_addr_max as u64 + 1
    };
    let upper_bound = guest_mem_size
        .min(protocol_limit)
        .min(BOOT_PARAM_ADDRESS_LIMIT);
    let unaligned_address = upper_bound
        .checked_sub(initrd_size)
        .ok_or("Guest memory is too small for the initrd")?;
    let address = unaligned_address & !(PAGE_SIZE - 1);
    let reserved_end = occupied_end
        .checked_add(PAGE_SIZE - 1)
        .ok_or("Linux reserved range overflowed")?
        & !(PAGE_SIZE - 1);
    if address < reserved_end {
        return Err("Guest memory is too small for the Linux kernel and initrd");
    }
    Ok(address)
}

fn load_image(vcpu: &mut dyn VCpu, image: &[u8], addr: usize) -> Result<(), &'static str> {
    info!(
        "Loading image at address {:#x}, size: {} bytes",
        addr,
        image.len()
    );
    vcpu.write_memory_slice(addr as u64, image)
}

pub const LAYOUT_BOOTPARAM: u64 = 0x0001_0000;
pub const LAYOUT_CMDLINE: u64 = 0x0002_0000;
pub const LAYOUT_KERNEL_BASE: u64 = 0x0010_0000;

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct BootParams {
    pub _screen_info: [u8; 0x40],
    pub _apm_bios_info: [u8; 0x14],
    pub _pad2: [u8; 4],
    pub tboot_addr: u64,
    pub ist_info: [u8; 0x10],
    pub _pad3: [u8; 0x10],
    pub hd0_info: [u8; 0x10],
    pub hd1_info: [u8; 0x10],
    pub _sys_desc_table: [u8; 0x10],
    pub _olpc_ofw_header: [u8; 0x10],
    pub _pad4: [u8; 0x80],
    pub _edid_info: [u8; 0x80],
    pub _efi_info: [u8; 0x20],
    pub alt_mem_k: u32,
    pub scratch: u32,
    pub e820_entries: u8,
    pub eddbuf_entries: u8,
    pub edd_mbr_sig_buf_entries: u8,
    pub kbd_status: u8,
    pub _pad6: [u8; 5],
    pub hdr: SetupHeader,
    pub _pad7: [u8; 0x290 - SetupHeader::HEADER_OFFSET - size_of::<SetupHeader>()],
    pub _edd_mbr_sig_buffer: [u32; 0x10],
    pub e820_map: [E820Entry; Self::E820MAX],
    pub _unimplemented: [u8; 0x330],
}

impl Default for BootParams {
    fn default() -> Self {
        Self::new()
    }
}

impl BootParams {
    pub const E820MAX: usize = 128;

    pub fn new() -> Self {
        Self {
            _screen_info: [0; 0x40],
            _apm_bios_info: [0; 0x14],
            _pad2: [0; 4],
            tboot_addr: 0,
            ist_info: [0; 0x10],
            _pad3: [0; 0x10],
            hd0_info: [0; 0x10],
            hd1_info: [0; 0x10],
            _sys_desc_table: [0; 0x10],
            _olpc_ofw_header: [0; 0x10],
            _pad4: [0; 0x80],
            _edid_info: [0; 0x80],
            _efi_info: [0; 0x20],
            alt_mem_k: 0,
            scratch: 0,
            e820_entries: 0,
            eddbuf_entries: 0,
            edd_mbr_sig_buf_entries: 0,
            kbd_status: 0,
            _pad6: [0; 5],
            hdr: SetupHeader::default(),
            _pad7: [0; 0x290 - SetupHeader::HEADER_OFFSET - size_of::<SetupHeader>()],
            _edd_mbr_sig_buffer: [0; 0x10],
            e820_map: [E820Entry {
                addr: 0,
                size: 0,
                type_: E820Type::Ram as u32,
            }; Self::E820MAX],
            _unimplemented: [0; 0x330],
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        let hdr = SetupHeader::from_bytes(bytes)?;
        let mut bp = BootParams::new();
        bp.hdr = hdr;
        Ok(bp)
    }

    pub fn add_e820_entry(&mut self, addr: u64, size: u64, type_: E820Type) {
        self.e820_map[self.e820_entries as usize].addr = addr;
        self.e820_map[self.e820_entries as usize].size = size;
        self.e820_map[self.e820_entries as usize].type_ = type_ as u32;
        self.e820_entries += 1;
    }
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SetupHeader {
    pub setup_sects: u8,
    pub root_flags: u16,
    pub syssize: u32,
    pub ram_size: u16,
    pub vid_mode: u16,
    pub root_dev: u16,
    pub boot_flag: u16,
    pub jump: u16,
    pub header: u32,
    pub version: u16,
    pub realmode_switch: u32,
    pub start_sys_seg: u16,
    pub kernel_version: u16,
    pub type_of_loader: u8,
    pub loadflags: LoadflagBitfield,
    pub setup_move_size: u16,
    pub code32_start: u32,
    pub ramdisk_image: u32,
    pub ramdisk_size: u32,
    pub bootsect_kludge: u32,
    pub heap_end_ptr: u16,
    pub ext_loader_ver: u8,
    pub ext_loader_type: u8,
    pub cmd_line_ptr: u32,
    pub initrd_addr_max: u32,
    pub kernel_alignment: u32,
    pub relocatable_kernel: u8,
    pub min_alignment: u8,
    pub xloadflags: u16,
    pub cmdline_size: u32,
    pub hardware_subarch: u32,
    pub hardware_subarch_data: u64,
    pub payload_offset: u32,
    pub payload_length: u32,
    pub setup_data: u64,
    pub pref_address: u64,
    pub init_size: u32,
    pub handover_offset: u32,
    pub kernel_info_offset: u32,
}

impl SetupHeader {
    pub const HEADER_OFFSET: usize = 0x1F1;

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < Self::HEADER_OFFSET + size_of::<Self>() {
            return Err("Binary data is too short to contain a valid SetupHeader");
        }

        let mut hdr = unsafe {
            let header_ptr = bytes.as_ptr().add(Self::HEADER_OFFSET) as *const Self;
            read_unaligned(header_ptr)
        };

        if hdr.setup_sects == 0 {
            hdr.setup_sects = 4;
        }

        Ok(hdr)
    }

    pub fn get_protected_code_offset(&self) -> usize {
        (self.setup_sects as usize + 1) * 512
    }
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LoadflagBitfield {
    raw: u8,
}

impl LoadflagBitfield {
    pub fn loaded_high(&self) -> bool {
        (self.raw & 0x01) != 0
    }

    pub fn set_loaded_high(&mut self, loaded_high: bool) {
        if loaded_high {
            self.raw |= 0x01;
        } else {
            self.raw &= !0x01;
        }
    }

    pub fn kaslr_flag(&self) -> bool {
        (self.raw & 0x02) != 0
    }

    pub fn quiet_flag(&self) -> bool {
        (self.raw & 0x20) != 0
    }

    pub fn keep_segments(&self) -> bool {
        (self.raw & 0x40) != 0
    }

    pub fn set_keep_segments(&mut self, keep_segments: bool) {
        if keep_segments {
            self.raw |= 0x40;
        } else {
            self.raw &= !0x40;
        }
    }

    pub fn can_use_heap(&self) -> bool {
        (self.raw & 0x80) != 0
    }

    pub fn set_can_use_heap(&mut self, can_use_heap: bool) {
        if can_use_heap {
            self.raw |= 0x80;
        } else {
            self.raw &= !0x80;
        }
    }

    pub fn new(
        loaded_high: bool,
        kaslr_flag: bool,
        quiet_flag: bool,
        keep_segments: bool,
        can_use_heap: bool,
    ) -> Self {
        let mut raw = 0u8;
        if loaded_high {
            raw |= 0x01;
        }
        if kaslr_flag {
            raw |= 0x02;
        }
        if quiet_flag {
            raw |= 0x20;
        }
        if keep_segments {
            raw |= 0x40;
        }
        if can_use_heap {
            raw |= 0x80;
        }
        Self { raw }
    }

    pub fn to_u8(&self) -> u8 {
        self.raw
    }
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct E820Entry {
    addr: u64,
    size: u64,
    type_: u32,
}

impl E820Entry {
    pub fn get_addr(&self) -> u64 {
        self.addr
    }

    pub fn get_size(&self) -> u64 {
        self.size
    }

    pub fn get_type(&self) -> Result<E820Type, &'static str> {
        match self.type_ {
            1 => Ok(E820Type::Ram),
            2 => Ok(E820Type::Reserved),
            3 => Ok(E820Type::Acpi),
            4 => Ok(E820Type::Nvs),
            5 => Ok(E820Type::Unusable),
            _ => Err("Invalid E820 type"),
        }
    }

    pub fn new(addr: u64, size: u64, type_: E820Type) -> Self {
        Self {
            addr,
            size,
            type_: type_ as u32,
        }
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum E820Type {
    Ram = 1,
    Reserved = 2,
    Acpi = 3,
    Nvs = 4,
    Unusable = 5,
}

#[cfg(test)]
mod tests {
    use super::choose_initrd_address;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn places_initrd_below_128_mib_guest_limit() {
        let size = 2_064_563;
        let address = choose_initrd_address(128 * MIB, u32::MAX, size, 60 * MIB).unwrap();

        assert_eq!(address & 0xfff, 0);
        assert!(address >= 60 * MIB);
        assert!(address + size <= 128 * MIB);
    }

    #[test]
    fn honors_linux_initrd_address_limit() {
        let size = 2 * MIB;
        let address =
            choose_initrd_address(128 * MIB, (64 * MIB - 1) as u32, size, 32 * MIB).unwrap();

        assert_eq!(address, 62 * MIB);
    }

    #[test]
    fn rejects_initrd_that_would_overlap_kernel_runtime() {
        let result = choose_initrd_address(128 * MIB, u32::MAX, 2 * MIB, 127 * MIB);

        assert_eq!(
            result,
            Err("Guest memory is too small for the Linux kernel and initrd")
        );
    }
}
