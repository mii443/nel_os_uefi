use core::{
    ptr::{read_unaligned, read_volatile, write_volatile},
    sync::atomic::{Ordering, fence},
};

use x86_64::{
    PhysAddr,
    instructions::port::Port,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use super::pci::{self, PciAddress};
use crate::{error, info};

const PAGE_SIZE: u64 = 4096;
const CONFIG_ADDRESS: u16 = 0x0cf8;
const CONFIG_DATA: u16 = 0x0cfc;
const GUEST_IRQ: u8 = 5;

const IOMMU_CAP: u64 = 0x08;
const IOMMU_GCMD: u64 = 0x18;
const IOMMU_GSTS: u64 = 0x1c;
const IOMMU_RTADDR: u64 = 0x20;
const IOMMU_CCMD: u64 = 0x28;
const IOMMU_FSTS: u64 = 0x34;
const IOMMU_ECAP: u64 = 0x10;
const GCMD_TE: u32 = 1 << 31;
const GCMD_SRTP: u32 = 1 << 30;

#[derive(Clone, Copy, Debug)]
pub struct PassthroughDescriptor {
    address: PciAddress,
    io_base: u16,
    iommu_base: u64,
    device_status_address: u64,
    bars: [u32; 6],
    bar_masks: [u32; 6],
}

impl PassthroughDescriptor {
    pub fn probe(rsdp: u64) -> Result<Self, &'static str> {
        let address =
            pci::find_nth_virtio_net(1).ok_or("a second virtio-net PCI device was not found")?;
        let io_base = pci::io_bar(address).unwrap_or(0);
        let iommu_base = find_dmar_register_base(rsdp)
            .ok_or("ACPI DMAR does not describe a segment-zero Intel IOMMU")?;
        let mut bars = [0u32; 6];
        let mut bar_masks = [0u32; 6];
        for index in 0..6 {
            let offset = 0x10 + index as u8 * 4;
            bars[index] = address.read_u32(offset);
            address.write_u32(offset, u32::MAX);
            bar_masks[index] = address.read_u32(offset);
            address.write_u32(offset, bars[index]);
        }
        let isr_address = find_virtio_cap_address(address, &bars, 3)
            .ok_or("the passthrough NIC has no virtio ISR capability")?;
        let common_config_address = find_virtio_cap_address(address, &bars, 1)
            .ok_or("the passthrough NIC has no virtio common configuration capability")?;
        let device_status_address = common_config_address + 20;

        // Keep the endpoint unable to DMA until its translation domain exists.
        let command = address.read_u16(0x04);
        address.write_u16(0x04, command & !(1 << 2));
        if io_base != 0 {
            unsafe { Port::<u8>::new(io_base + 18).write(0) };
        } else {
            unsafe { write_volatile(device_status_address as *mut u8, 0) };
        }

        info!(
            "Reserved virtio-net {:02x}:{:02x}.{} for VM 0 PCI passthrough (ISR {:#x}, VT-d {:#x})",
            address.bus, address.device, address.function, isr_address, iommu_base
        );
        Ok(Self {
            address,
            io_base,
            iommu_base,
            device_status_address,
            bars,
            bar_masks,
        })
    }
}

pub struct PassthroughNic {
    descriptor: PassthroughDescriptor,
    config_address: u32,
    command: u16,
    bar_probe: [bool; 6],
    fault_reported: bool,
    dma: DmaDomain,
}

impl PassthroughNic {
    pub fn new(
        descriptor: PassthroughDescriptor,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<Self, &'static str> {
        let dma = DmaDomain::new(descriptor, allocator)?;
        Ok(Self {
            descriptor,
            config_address: 0,
            command: 0,
            bar_probe: [false; 6],
            fault_reported: false,
            dma,
        })
    }

    pub fn map_dma(
        &mut self,
        guest_address: u64,
        host_address: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        self.dma.map_4k(guest_address, host_address, allocator)
    }

    pub fn mmio_regions(&self) -> [(u64, u64); 6] {
        let mut regions = [(0, 0); 6];
        let mut index = 0usize;
        while index < self.descriptor.bars.len() {
            let bar = self.descriptor.bars[index];
            let mask = self.descriptor.bar_masks[index];
            if bar & 1 == 0 && mask != 0 && mask != u32::MAX {
                let is_64 = bar & 0x6 == 0x4 && index + 1 < self.descriptor.bars.len();
                let mut base = u64::from(bar & !0xf);
                let mut size_mask = u64::from(mask & !0xf);
                if is_64 {
                    base |= u64::from(self.descriptor.bars[index + 1]) << 32;
                    size_mask |= u64::from(self.descriptor.bar_masks[index + 1]) << 32;
                }
                let size = (!size_mask).wrapping_add(1);
                if base != 0 && size != 0 {
                    regions[index] = (base, size);
                }
                if is_64 {
                    index += 1;
                }
            }
            index += 1;
        }
        regions
    }

    pub fn handles_port(&self, port: u16) -> bool {
        (CONFIG_ADDRESS..=CONFIG_ADDRESS + 3).contains(&port)
            || (CONFIG_DATA..=CONFIG_DATA + 3).contains(&port)
            || (self.descriptor.io_base != 0
                && (self.descriptor.io_base..self.descriptor.io_base + 0x20).contains(&port))
    }

    pub fn io_in(&mut self, port: u16, size: u8) -> u32 {
        if (CONFIG_ADDRESS..=CONFIG_ADDRESS + 3).contains(&port) {
            return extract(self.config_address, port - CONFIG_ADDRESS, size);
        }
        if (CONFIG_DATA..=CONFIG_DATA + 3).contains(&port) {
            return self.config_read(port - CONFIG_DATA, size);
        }
        if self.descriptor.io_base != 0
            && (self.descriptor.io_base..self.descriptor.io_base + 0x20).contains(&port)
        {
            return unsafe { port_read(port, size) };
        }
        width_mask(size)
    }

    pub fn io_out(&mut self, port: u16, size: u8, value: u32) {
        if (CONFIG_ADDRESS..=CONFIG_ADDRESS + 3).contains(&port) {
            merge(&mut self.config_address, port - CONFIG_ADDRESS, size, value);
        } else if (CONFIG_DATA..=CONFIG_DATA + 3).contains(&port) {
            self.config_write(port - CONFIG_DATA, size, value);
        } else if self.descriptor.io_base != 0
            && (self.descriptor.io_base..self.descriptor.io_base + 0x20).contains(&port)
        {
            unsafe { port_write(port, size, value) };
        }
    }

    pub fn poll_interrupt(&mut self) -> Option<u8> {
        let fault_status = unsafe { mmio_read_u32(self.descriptor.iommu_base + IOMMU_FSTS) };
        if fault_status != 0 && !self.fault_reported {
            error!(
                "VT-d reported a passthrough DMA fault: FSTS={:#x}",
                fault_status
            );
            self.fault_reported = true;
        }
        if self.command & 2 == 0 {
            return None;
        }
        // Reading the virtio ISR here would acknowledge it before the guest
        // driver sees it. Inject a polled virtual INTx instead; the guest reads
        // and filters the real ISR MMIO byte, including harmless spurious polls.
        (self.command & 0x6 == 0x6).then_some(GUEST_IRQ)
    }

    pub fn reset(&mut self) {
        self.config_address = 0;
        self.command = 0;
        self.bar_probe = [false; 6];
        self.fault_reported = false;
        if self.descriptor.io_base != 0 {
            unsafe { Port::<u8>::new(self.descriptor.io_base + 18).write(0) };
        } else {
            unsafe { write_volatile(self.descriptor.device_status_address as *mut u8, 0) };
        }
        self.apply_command();
    }

    fn selected_offset(&self, data_offset: u16) -> Option<u8> {
        if self.config_address & (1 << 31) == 0 {
            return None;
        }
        let selected = self.config_address & 0x00ff_ff00;
        let physical = self.descriptor.address.config_address(0) & 0x00ff_ff00;
        (selected == physical)
            .then_some(((self.config_address & 0xfc) as u8).wrapping_add(data_offset as u8))
    }

    fn config_read(&self, data_offset: u16, size: u8) -> u32 {
        let Some(offset) = self.selected_offset(data_offset) else {
            return width_mask(size);
        };
        if offset == 0x04 && size >= 2 {
            let physical = self.descriptor.address.read_u32(0x04);
            return physical & 0xffff_0000 | u32::from(self.command);
        }
        if (0x10..=0x24).contains(&offset) && offset & 3 == 0 && size == 4 {
            let index = ((offset - 0x10) / 4) as usize;
            return if self.bar_probe[index] {
                self.descriptor.bar_masks[index]
            } else {
                self.descriptor.bars[index]
            };
        }
        if offset == 0x3c {
            let mut value = self.descriptor.address.read_u32(0x3c);
            value = (value & !0xff) | u32::from(GUEST_IRQ);
            return value & width_mask(size);
        }
        extract(
            self.descriptor.address.read_u32(offset & 0xfc),
            u16::from(offset & 3),
            size,
        )
    }

    fn config_write(&mut self, data_offset: u16, size: u8, value: u32) {
        let Some(offset) = self.selected_offset(data_offset) else {
            return;
        };
        if offset == 0x04 {
            let old_command = self.command;
            let mut command = u32::from(self.command);
            merge(&mut command, 0, size.min(2), value);
            self.command = command as u16 & 0x0007;
            if old_command & (1 << 2) == 0 && self.command & (1 << 2) != 0 {
                fence(Ordering::SeqCst);
                if let Err(error) = self.dma.invalidate() {
                    error!("Unable to invalidate VT-d translations: {}", error);
                }
            }
            self.apply_command();
        } else if (0x10..=0x24).contains(&offset) && offset & 3 == 0 && size == 4 {
            let index = ((offset - 0x10) / 4) as usize;
            self.bar_probe[index] = value == u32::MAX;
        }
    }

    fn apply_command(&self) {
        const IO_SPACE: u16 = 1 << 0;
        const MEMORY_SPACE: u16 = 1 << 1;
        const BUS_MASTER: u16 = 1 << 2;
        const INTERRUPT_DISABLE: u16 = 1 << 10;
        let physical = self.descriptor.address.read_u16(0x04);
        let requested = self.command & (IO_SPACE | MEMORY_SPACE | BUS_MASTER);
        self.descriptor.address.write_u16(
            0x04,
            (physical & !(IO_SPACE | MEMORY_SPACE | BUS_MASTER)) | requested | INTERRUPT_DISABLE,
        );
    }
}

fn find_virtio_cap_address(address: PciAddress, bars: &[u32; 6], config_type: u8) -> Option<u64> {
    const PCI_CAP_ID_VNDR: u8 = 0x09;
    let mut capability = address.read_u8(0x34) & 0xfc;
    for _ in 0..48 {
        if capability < 0x40 {
            return None;
        }
        if address.read_u8(capability) == PCI_CAP_ID_VNDR
            && address.read_u8(capability + 3) == config_type
        {
            let bar_index = address.read_u8(capability + 4) as usize;
            if bar_index >= bars.len() || bars[bar_index] & 1 != 0 {
                return None;
            }
            let mut base = u64::from(bars[bar_index] & !0xf);
            if bars[bar_index] & 0x6 == 0x4 && bar_index + 1 < bars.len() {
                base |= u64::from(bars[bar_index + 1]) << 32;
            }
            let offset = u64::from(address.read_u32(capability + 8));
            return Some(base + offset);
        }
        capability = address.read_u8(capability + 1) & 0xfc;
    }
    None
}

struct DmaDomain {
    root: PhysFrame,
    levels: u8,
    iommu_base: u64,
}

impl DmaDomain {
    fn new(
        descriptor: PassthroughDescriptor,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<Self, &'static str> {
        let cap = unsafe { mmio_read_u64(descriptor.iommu_base + IOMMU_CAP) };
        let sagaw = ((cap >> 8) & 0x1f) as u8;
        let (agaw, levels) = if sagaw & (1 << 2) != 0 {
            (2u64, 4u8)
        } else if sagaw & (1 << 1) != 0 {
            (1, 3)
        } else if sagaw & 1 != 0 {
            (0, 2)
        } else {
            return Err("Intel IOMMU supports no usable adjusted guest address width");
        };
        let root = allocate_zeroed(allocator, "no frame for the VT-d second-level root")?;
        let root_table = allocate_zeroed(allocator, "no frame for the VT-d root table")?;

        for bus in 0..=u8::MAX {
            if !pci::bus_has_device(bus) {
                continue;
            }
            let context = allocate_zeroed(allocator, "no frame for a VT-d context table")?;
            let entries = table(context);
            // Devices retained by the outer hypervisor use VT-d pass-through.
            // Only the assigned NIC receives the translated DMA domain.
            for devfn in 0..256usize {
                entries[devfn * 2] = 1 | (2 << 2);
                entries[devfn * 2 + 1] = agaw | (1 << 8);
            }
            if bus == descriptor.address.bus {
                let devfn =
                    descriptor.address.device as usize * 8 + descriptor.address.function as usize;
                entries[devfn * 2] = root.start_address().as_u64() | 1;
                entries[devfn * 2 + 1] = agaw | (2 << 8);
            }
            let roots = table(root_table);
            roots[bus as usize * 2] = context.start_address().as_u64() | 1;
        }

        fence(Ordering::SeqCst);
        unsafe {
            mmio_write_u64(
                descriptor.iommu_base + IOMMU_RTADDR,
                root_table.start_address().as_u64(),
            );
            mmio_write_u32(descriptor.iommu_base + IOMMU_GCMD, GCMD_SRTP);
            wait_status(descriptor.iommu_base, GCMD_SRTP)?;
            mmio_write_u32(descriptor.iommu_base + IOMMU_GCMD, GCMD_TE);
            wait_status(descriptor.iommu_base, GCMD_TE)?;
        }
        info!(
            "VT-d DMA translation enabled for passthrough NIC ({} levels)",
            levels
        );
        Ok(Self {
            root,
            levels,
            iommu_base: descriptor.iommu_base,
        })
    }

    fn map_4k(
        &mut self,
        iova: u64,
        host_address: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        if iova & (PAGE_SIZE - 1) != 0 || host_address & (PAGE_SIZE - 1) != 0 {
            return Err("VT-d DMA mapping is not page aligned");
        }
        let mut current = self.root;
        for level in (2..=self.levels).rev() {
            let shift = 12 + 9 * (u64::from(level) - 1);
            let index = ((iova >> shift) & 0x1ff) as usize;
            let entries = table(current);
            if entries[index] & 1 == 0 {
                let next = allocate_zeroed(allocator, "no frame for a VT-d page table")?;
                entries[index] = next.start_address().as_u64() | 3;
                current = next;
            } else {
                current = PhysFrame::from_start_address(PhysAddr::new(entries[index] & !0xfff))
                    .map_err(|_| "invalid VT-d page-table address")?;
            }
        }
        table(current)[((iova >> 12) & 0x1ff) as usize] = host_address | 3;
        Ok(())
    }

    fn invalidate(&mut self) -> Result<(), &'static str> {
        const INVALIDATE: u64 = 1 << 63;
        const GLOBAL_CONTEXT: u64 = 1 << 61;
        const GLOBAL_IOTLB: u64 = 1 << 60;
        let base = self.iommu_base();
        unsafe {
            mmio_write_u64(base + IOMMU_CCMD, INVALIDATE | GLOBAL_CONTEXT);
            wait_u64_clear(base + IOMMU_CCMD, INVALIDATE)?;

            let ecap = mmio_read_u64(base + IOMMU_ECAP);
            let iotlb_offset = ((ecap >> 8) & 0x3ff) * 16;
            if iotlb_offset == 0 {
                return Err("Intel IOMMU exposes no IOTLB invalidation register");
            }
            let iotlb = base + iotlb_offset + 8;
            mmio_write_u64(iotlb, INVALIDATE | GLOBAL_IOTLB);
            wait_u64_clear(iotlb, INVALIDATE)?;
        }
        Ok(())
    }

    fn iommu_base(&self) -> u64 {
        // The QEMU DMAR register base is stable and globally unique. Recover
        // it from the domain metadata stored next to the paging root.
        self.iommu_base
    }
}

fn allocate_zeroed(
    allocator: &mut dyn FrameAllocator<Size4KiB>,
    error: &'static str,
) -> Result<PhysFrame, &'static str> {
    let frame = allocator.allocate_frame().ok_or(error)?;
    unsafe {
        core::ptr::write_bytes(
            frame.start_address().as_u64() as *mut u8,
            0,
            PAGE_SIZE as usize,
        )
    };
    Ok(frame)
}

fn table(frame: PhysFrame) -> &'static mut [u64; 512] {
    unsafe { &mut *(frame.start_address().as_u64() as *mut [u64; 512]) }
}

unsafe fn wait_status(base: u64, bit: u32) -> Result<(), &'static str> {
    for _ in 0..1_000_000 {
        if unsafe { mmio_read_u32(base + IOMMU_GSTS) } & bit != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err("Intel IOMMU command timed out")
}

unsafe fn wait_u64_clear(address: u64, bit: u64) -> Result<(), &'static str> {
    for _ in 0..1_000_000 {
        if unsafe { mmio_read_u64(address) } & bit == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err("Intel IOMMU invalidation timed out")
}

unsafe fn mmio_read_u32(address: u64) -> u32 {
    unsafe { read_volatile(address as *const u32) }
}

unsafe fn mmio_read_u64(address: u64) -> u64 {
    unsafe { read_volatile(address as *const u64) }
}

unsafe fn mmio_write_u32(address: u64, value: u32) {
    unsafe { write_volatile(address as *mut u32, value) }
}

unsafe fn mmio_write_u64(address: u64, value: u64) {
    unsafe { write_volatile(address as *mut u64, value) }
}

unsafe fn port_read(port: u16, size: u8) -> u32 {
    match size {
        1 => unsafe { Port::<u8>::new(port).read() as u32 },
        2 => unsafe { Port::<u16>::new(port).read() as u32 },
        4 => unsafe { Port::<u32>::new(port).read() },
        _ => u32::MAX,
    }
}

unsafe fn port_write(port: u16, size: u8, value: u32) {
    match size {
        1 => unsafe { Port::<u8>::new(port).write(value as u8) },
        2 => unsafe { Port::<u16>::new(port).write(value as u16) },
        4 => unsafe { Port::<u32>::new(port).write(value) },
        _ => {}
    }
}

fn width_mask(size: u8) -> u32 {
    match size {
        1 => u8::MAX as u32,
        2 => u16::MAX as u32,
        _ => u32::MAX,
    }
}

fn extract(value: u32, byte_offset: u16, size: u8) -> u32 {
    (value >> (u32::from(byte_offset) * 8)) & width_mask(size)
}

fn merge(target: &mut u32, byte_offset: u16, size: u8, value: u32) {
    let shift = u32::from(byte_offset) * 8;
    let mask = width_mask(size) << shift;
    *target = (*target & !mask) | ((value << shift) & mask);
}

fn find_dmar_register_base(rsdp: u64) -> Option<u64> {
    unsafe {
        if rsdp == 0 || core::slice::from_raw_parts(rsdp as *const u8, 8) != b"RSD PTR " {
            return None;
        }
        let revision = *((rsdp + 15) as *const u8);
        let (root, entry_size) = if revision >= 2 {
            (read_unaligned((rsdp + 24) as *const u64), 8usize)
        } else {
            (u64::from(read_unaligned((rsdp + 16) as *const u32)), 4usize)
        };
        let length = read_unaligned((root + 4) as *const u32) as usize;
        if length < 36 || length > 1024 * 1024 {
            return None;
        }
        let entries = (length - 36) / entry_size;
        for index in 0..entries {
            let pointer = root + 36 + (index * entry_size) as u64;
            let table_address = if entry_size == 8 {
                read_unaligned(pointer as *const u64)
            } else {
                u64::from(read_unaligned(pointer as *const u32))
            };
            if core::slice::from_raw_parts(table_address as *const u8, 4) != b"DMAR" {
                continue;
            }
            let table_length = read_unaligned((table_address + 4) as *const u32) as usize;
            if table_length < 48 || table_length > 1024 * 1024 {
                return None;
            }
            let mut offset = 48usize;
            while offset + 16 <= table_length {
                let structure = table_address + offset as u64;
                let typ = read_unaligned(structure as *const u16);
                let structure_length = read_unaligned((structure + 2) as *const u16) as usize;
                if structure_length < 4 || offset + structure_length > table_length {
                    return None;
                }
                // QEMU may describe endpoint scopes instead of setting the
                // INCLUDE_PCI_ALL flag. The register block still controls the
                // complete segment-zero translation root programmed here.
                if typ == 0 && read_unaligned((structure + 6) as *const u16) == 0 {
                    return Some(read_unaligned((structure + 8) as *const u64));
                }
                offset += structure_length;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_subword_helpers_preserve_other_bytes() {
        let mut value = 0x1122_3344;
        merge(&mut value, 1, 1, 0xaa);
        assert_eq!(value, 0x1122_aa44);
        assert_eq!(extract(value, 1, 2), 0x22aa);
    }
}
