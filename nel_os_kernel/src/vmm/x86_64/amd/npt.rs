use x86_64::{
    PhysAddr,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::{memory::bitmap::BitmapMemoryTable, storage::GuestMemory, vmm::x86_64::common::uefi};

const ENTRY_PRESENT: u64 = 1 << 0;
const ENTRY_WRITABLE: u64 = 1 << 1;
const ENTRY_USER: u64 = 1 << 2;
const ENTRY_ACCESSED: u64 = 1 << 5;
const ENTRY_HUGE: u64 = 1 << 7;
const ENTRY_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const TABLE_FLAGS: u64 = ENTRY_PRESENT | ENTRY_WRITABLE | ENTRY_USER;

pub struct Npt {
    pub root_table: PhysFrame,
}

impl Npt {
    pub fn new(allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let root_table = allocator
            .allocate_frame()
            .ok_or("Failed to allocate NPT root table")?;
        Self::clear_frame(root_table);
        Ok(Self { root_table })
    }

    pub fn map_4k(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        if gpa & 0xfff != 0 || hpa & 0xfff != 0 {
            return Err("NPT mapping is not page aligned");
        }

        let indexes = [
            ((gpa >> 39) & 0x1ff) as usize,
            ((gpa >> 30) & 0x1ff) as usize,
            ((gpa >> 21) & 0x1ff) as usize,
        ];
        let mut table_frame = self.root_table;

        for index in indexes {
            let table = Self::frame_to_table(table_frame);
            let entry = &mut table[index];
            if *entry & ENTRY_PRESENT == 0 {
                let next = allocator
                    .allocate_frame()
                    .ok_or("Failed to allocate NPT page table")?;
                Self::clear_frame(next);
                *entry = next.start_address().as_u64() | TABLE_FLAGS;
                table_frame = next;
            } else {
                if *entry & ENTRY_HUGE != 0 {
                    return Err("NPT mapping collides with a huge page");
                }
                table_frame =
                    PhysFrame::from_start_address(PhysAddr::new(*entry & ENTRY_ADDRESS_MASK))
                        .map_err(|_| "Invalid NPT table address")?;
            }
        }

        let table = Self::frame_to_table(table_frame);
        table[((gpa >> 12) & 0x1ff) as usize] = hpa | TABLE_FLAGS;
        Ok(())
    }

    pub fn reserve_4k(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        self.map_4k(gpa, hpa, allocator)?;
        let indexes = [
            ((gpa >> 39) & 0x1ff) as usize,
            ((gpa >> 30) & 0x1ff) as usize,
            ((gpa >> 21) & 0x1ff) as usize,
        ];
        let mut table_frame = self.root_table;
        for index in indexes {
            let entry = Self::frame_to_table(table_frame)[index];
            table_frame = Self::table_frame(entry).ok_or("Invalid reserved NPT table")?;
        }
        Self::frame_to_table(table_frame)[((gpa >> 12) & 0x1ff) as usize] &= !TABLE_FLAGS;
        Ok(())
    }

    pub fn map_2m(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        if gpa & 0x1f_ffff != 0 || hpa & 0x1f_ffff != 0 {
            return Err("NPT 2 MiB mapping is not aligned");
        }

        let indexes = [
            ((gpa >> 39) & 0x1ff) as usize,
            ((gpa >> 30) & 0x1ff) as usize,
        ];
        let mut table_frame = self.root_table;
        for index in indexes {
            let table = Self::frame_to_table(table_frame);
            let entry = &mut table[index];
            if *entry & ENTRY_PRESENT == 0 {
                let next = allocator
                    .allocate_frame()
                    .ok_or("Failed to allocate NPT page table")?;
                Self::clear_frame(next);
                *entry = next.start_address().as_u64() | TABLE_FLAGS;
                table_frame = next;
            } else {
                if *entry & ENTRY_HUGE != 0 {
                    return Err("NPT mapping collides with a huge page");
                }
                table_frame =
                    PhysFrame::from_start_address(PhysAddr::new(*entry & ENTRY_ADDRESS_MASK))
                        .map_err(|_| "Invalid NPT table address")?;
            }
        }

        let table = Self::frame_to_table(table_frame);
        let entry = &mut table[((gpa >> 21) & 0x1ff) as usize];
        if *entry & ENTRY_PRESENT != 0 {
            return Err("NPT 2 MiB mapping collides with an existing mapping");
        }
        *entry = hpa | TABLE_FLAGS | ENTRY_HUGE;
        Ok(())
    }

    pub fn reserve_2m(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        self.map_2m(gpa, hpa, allocator)?;
        let indexes = [
            ((gpa >> 39) & 0x1ff) as usize,
            ((gpa >> 30) & 0x1ff) as usize,
        ];
        let mut table_frame = self.root_table;
        for index in indexes {
            let entry = Self::frame_to_table(table_frame)[index];
            table_frame = Self::table_frame(entry).ok_or("Invalid reserved NPT table")?;
        }
        Self::frame_to_table(table_frame)[((gpa >> 21) & 0x1ff) as usize] &= !TABLE_FLAGS;
        Ok(())
    }

    /// Makes an already-backed, CPU-hidden DMA mapping visible after its first
    /// nested-page fault. Intermediate page-table entries remain present so the
    /// reserved HPA can be recovered without a side allocation table.
    pub fn activate_reserved(&mut self, gpa: u64) -> bool {
        let indexes = [
            ((gpa >> 39) & 0x1ff) as usize,
            ((gpa >> 30) & 0x1ff) as usize,
            ((gpa >> 21) & 0x1ff) as usize,
            ((gpa >> 12) & 0x1ff) as usize,
        ];
        let mut table_frame = self.root_table;

        for (level, index) in indexes.into_iter().enumerate() {
            let entry = &mut Self::frame_to_table(table_frame)[index];
            let terminal = (level == 2 && *entry & ENTRY_HUGE != 0) || level == 3;
            if terminal && *entry & ENTRY_ADDRESS_MASK != 0 {
                if *entry & ENTRY_PRESENT != 0 {
                    return false;
                }
                *entry |= TABLE_FLAGS;
                return true;
            }
            if *entry & ENTRY_PRESENT == 0 || *entry & ENTRY_HUGE != 0 {
                return false;
            }
            let Ok(next) =
                PhysFrame::from_start_address(PhysAddr::new(*entry & ENTRY_ADDRESS_MASK))
            else {
                return false;
            };
            table_frame = next;
        }
        false
    }

    pub fn get_backing_phys_addr(&self, gpa: u64) -> Option<u64> {
        let indexes = [
            ((gpa >> 39) & 0x1ff) as usize,
            ((gpa >> 30) & 0x1ff) as usize,
            ((gpa >> 21) & 0x1ff) as usize,
            ((gpa >> 12) & 0x1ff) as usize,
        ];
        let mut table_frame = self.root_table;

        for (level, index) in indexes.into_iter().enumerate() {
            let entry = Self::frame_to_table(table_frame)[index];
            if entry & ENTRY_HUGE != 0 {
                let offset_mask = if level == 1 { 0x3fff_ffff } else { 0x1f_ffff };
                return Some((entry & ENTRY_ADDRESS_MASK & !offset_mask) | (gpa & offset_mask));
            }
            if level == 3 {
                return (entry & ENTRY_ADDRESS_MASK != 0)
                    .then_some((entry & ENTRY_ADDRESS_MASK) | (gpa & 0xfff));
            }
            if entry & ENTRY_PRESENT == 0 {
                return None;
            }
            table_frame =
                PhysFrame::from_start_address(PhysAddr::new(entry & ENTRY_ADDRESS_MASK)).ok()?;
        }
        None
    }

    pub fn get_phys_addr(&self, gpa: u64) -> Option<u64> {
        let indexes = [
            ((gpa >> 39) & 0x1ff) as usize,
            ((gpa >> 30) & 0x1ff) as usize,
            ((gpa >> 21) & 0x1ff) as usize,
            ((gpa >> 12) & 0x1ff) as usize,
        ];
        let mut table_frame = self.root_table;

        for (level, index) in indexes.into_iter().enumerate() {
            let entry = Self::frame_to_table(table_frame)[index];
            if entry & ENTRY_PRESENT == 0 {
                return None;
            }

            if entry & ENTRY_HUGE != 0 {
                let offset_mask = if level == 1 { 0x3fff_ffff } else { 0x1f_ffff };
                return Some((entry & ENTRY_ADDRESS_MASK & !offset_mask) | (gpa & offset_mask));
            }

            if level == 3 {
                return Some((entry & ENTRY_ADDRESS_MASK) | (gpa & 0xfff));
            }

            table_frame =
                PhysFrame::from_start_address(PhysAddr::new(entry & ENTRY_ADDRESS_MASK)).ok()?;
        }

        None
    }

    fn is_accessed(&self, gpa: u64) -> bool {
        let indexes = [
            ((gpa >> 39) & 0x1ff) as usize,
            ((gpa >> 30) & 0x1ff) as usize,
            ((gpa >> 21) & 0x1ff) as usize,
            ((gpa >> 12) & 0x1ff) as usize,
        ];
        let mut table_frame = self.root_table;

        for (level, index) in indexes.into_iter().enumerate() {
            let entry = Self::frame_to_table(table_frame)[index];
            if entry & ENTRY_PRESENT == 0 {
                return false;
            }
            if entry & ENTRY_HUGE != 0 || level == 3 {
                return entry & ENTRY_ACCESSED != 0;
            }
            let Ok(frame) =
                PhysFrame::from_start_address(PhysAddr::new(entry & ENTRY_ADDRESS_MASK))
            else {
                return false;
            };
            table_frame = frame;
        }

        false
    }

    pub fn accessed_bytes(&self, gpa_start: u64, gpa_end: u64) -> u64 {
        let first_page = gpa_start / 4096;
        let page_count = gpa_end.div_ceil(4096);
        (first_page..page_count)
            .filter(|page| self.is_accessed(page * 4096))
            .count() as u64
            * 4096
    }

    pub fn reclaim(self, allocator: &mut BitmapMemoryTable, guest_memory_size: u64) {
        let lv4_table = Self::frame_to_table(self.root_table);
        for (lv4_index, &lv4_entry) in lv4_table.iter().enumerate() {
            let Some(lv3_frame) = Self::table_frame(lv4_entry) else {
                continue;
            };
            let lv3_table = Self::frame_to_table(lv3_frame);
            for (lv3_index, &lv3_entry) in lv3_table.iter().enumerate() {
                let Some(lv2_frame) = Self::table_frame(lv3_entry) else {
                    continue;
                };
                let lv2_table = Self::frame_to_table(lv2_frame);
                for (lv2_index, &lv2_entry) in lv2_table.iter().enumerate() {
                    if lv2_entry & ENTRY_PRESENT == 0
                        && (lv2_entry & ENTRY_HUGE == 0 || lv2_entry & ENTRY_ADDRESS_MASK == 0)
                    {
                        continue;
                    }
                    let gpa_2m = (lv4_index as u64) << 39
                        | (lv3_index as u64) << 30
                        | (lv2_index as u64) << 21;
                    if lv2_entry & ENTRY_HUGE != 0 {
                        if uefi::ram_range_containing(guest_memory_size, gpa_2m).is_some() {
                            allocator.deallocate_contiguous_frames(
                                PhysFrame::containing_address(PhysAddr::new(
                                    lv2_entry & ENTRY_ADDRESS_MASK,
                                )),
                                512,
                            );
                        }
                        continue;
                    }
                    let Some(lv1_frame) = Self::table_frame(lv2_entry) else {
                        continue;
                    };
                    let lv1_table = Self::frame_to_table(lv1_frame);
                    for (lv1_index, &lv1_entry) in lv1_table.iter().enumerate() {
                        let gpa = gpa_2m | (lv1_index as u64) << 12;
                        if lv1_entry & ENTRY_ADDRESS_MASK != 0
                            && uefi::owns_backing_page(guest_memory_size, gpa)
                        {
                            allocator.deallocate_frame(PhysFrame::containing_address(
                                PhysAddr::new(lv1_entry & ENTRY_ADDRESS_MASK),
                            ));
                        }
                    }
                    allocator.deallocate_frame(lv1_frame);
                }
                allocator.deallocate_frame(lv2_frame);
            }
            allocator.deallocate_frame(lv3_frame);
        }
        allocator.deallocate_frame(self.root_table);
    }

    fn table_frame(entry: u64) -> Option<PhysFrame<Size4KiB>> {
        if entry & ENTRY_PRESENT == 0 || entry & ENTRY_HUGE != 0 {
            return None;
        }
        PhysFrame::from_start_address(PhysAddr::new(entry & ENTRY_ADDRESS_MASK)).ok()
    }

    pub fn get(&self, gpa: u64) -> Result<u8, &'static str> {
        let hpa = self
            .get_backing_phys_addr(gpa)
            .ok_or("Guest physical address is not mapped")?;
        Ok(unsafe { *(hpa as *const u8) })
    }

    pub fn set(&mut self, gpa: u64, value: u8) -> Result<(), &'static str> {
        let hpa = self
            .get_backing_phys_addr(gpa)
            .ok_or("Guest physical address is not mapped")?;
        unsafe { *(hpa as *mut u8) = value };
        Ok(())
    }

    pub fn set_range(
        &mut self,
        gpa_start: u64,
        gpa_end: u64,
        value: u8,
    ) -> Result<(), &'static str> {
        if gpa_start > gpa_end {
            return Err("Invalid guest physical address range");
        }

        let mut gpa = gpa_start;
        while gpa < gpa_end {
            let hpa = self
                .get_backing_phys_addr(gpa)
                .ok_or("Guest physical address is not mapped")?;
            let bytes = ((0x1000 - (gpa & 0xfff)).min(gpa_end - gpa)) as usize;
            unsafe { core::ptr::write_bytes(hpa as *mut u8, value, bytes) };
            gpa += bytes as u64;
        }
        Ok(())
    }

    pub fn set_slice(&mut self, gpa_start: u64, data: &[u8]) -> Result<(), &'static str> {
        let mut gpa = gpa_start;
        let mut offset = 0;
        while offset < data.len() {
            let hpa = self
                .get_backing_phys_addr(gpa)
                .ok_or("Guest physical address is not mapped")?;
            let bytes = (0x1000 - (gpa as usize & 0xfff)).min(data.len() - offset);
            unsafe {
                core::ptr::copy_nonoverlapping(data[offset..].as_ptr(), hpa as *mut u8, bytes)
            };
            gpa += bytes as u64;
            offset += bytes;
        }
        Ok(())
    }

    pub fn get_slice(&self, gpa_start: u64, output: &mut [u8]) -> Result<(), &'static str> {
        let mut gpa = gpa_start;
        let mut offset = 0;
        while offset < output.len() {
            let hpa = self
                .get_backing_phys_addr(gpa)
                .ok_or("Guest physical address is not mapped")?;
            let bytes = (0x1000 - (gpa as usize & 0xfff)).min(output.len() - offset);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    hpa as *const u8,
                    output[offset..].as_mut_ptr(),
                    bytes,
                )
            };
            gpa += bytes as u64;
            offset += bytes;
        }
        Ok(())
    }

    fn clear_frame(frame: PhysFrame) {
        unsafe {
            core::ptr::write_bytes(frame.start_address().as_u64() as *mut u8, 0, 4096);
        }
    }

    fn frame_to_table(frame: PhysFrame) -> &'static mut [u64; 512] {
        unsafe { &mut *(frame.start_address().as_u64() as *mut [u64; 512]) }
    }
}

impl GuestMemory for Npt {
    fn read_u8(&mut self, address: u64) -> Result<u8, &'static str> {
        self.get(address)
    }

    fn write_u8(&mut self, address: u64, value: u8) -> Result<(), &'static str> {
        self.set(address, value)
    }

    fn read_slice(&mut self, address: u64, output: &mut [u8]) -> Result<(), &'static str> {
        self.get_slice(address, output)
    }

    fn write_slice(&mut self, address: u64, input: &[u8]) -> Result<(), &'static str> {
        self.set_slice(address, input)
    }
}
