use x86_64::{
    PhysAddr,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::storage::GuestMemory;

const ENTRY_PRESENT: u64 = 1 << 0;
const ENTRY_WRITABLE: u64 = 1 << 1;
const ENTRY_USER: u64 = 1 << 2;
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

    pub fn get(&self, gpa: u64) -> Result<u8, &'static str> {
        let hpa = self
            .get_phys_addr(gpa)
            .ok_or("Guest physical address is not mapped")?;
        Ok(unsafe { *(hpa as *const u8) })
    }

    pub fn set(&mut self, gpa: u64, value: u8) -> Result<(), &'static str> {
        let hpa = self
            .get_phys_addr(gpa)
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
                .get_phys_addr(gpa)
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
                .get_phys_addr(gpa)
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

    fn write_slice(&mut self, address: u64, input: &[u8]) -> Result<(), &'static str> {
        self.set_slice(address, input)
    }
}
