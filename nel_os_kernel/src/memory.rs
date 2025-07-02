use nel_os_common::memory::{self, UsableMemory};
use x86_64::{
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
    PhysAddr,
};

use crate::constant::{BITS_PER_ENTRY, ENTRY_COUNT, PAGE_SIZE};

pub struct BitmapMemoryTable {
    pub used_map: [usize; ENTRY_COUNT],
    pub start: usize,
    pub end: usize,
}

impl BitmapMemoryTable {
    pub fn new() -> Self {
        Self {
            used_map: [0; ENTRY_COUNT],
            start: 0,
            end: usize::MAX,
        }
    }

    pub fn init(usable_memory: &UsableMemory) -> Self {
        let mut table = Self::default();
        for range in usable_memory.ranges() {
            table.set_range(range);
        }

        for i in 0..ENTRY_COUNT {
            let index = ENTRY_COUNT - i - 1;
            if table.used_map[index] != 0 {
                let offset = 63 - table.used_map[index].leading_zeros();
                table.end = (index + 1) * BITS_PER_ENTRY + offset as usize;
                break;
            }
        }

        table
    }

    pub fn get_free_pfn(&self) -> Option<usize> {
        (self.start..self.end).find(|&i| self.get_bit(i))
    }

    pub fn set_range(&mut self, range: &memory::Range) {
        let start = Self::addr_to_pfn(range.start as usize);
        let size = (range.end - range.start) / PAGE_SIZE as u64;

        for i in 0..size {
            self.set_frame(start + i as usize, true);
        }
    }

    pub fn set_frame(&mut self, frame: usize, state: bool) {
        let index = Self::frame_to_index(frame);
        let offset = Self::frame_to_offset(frame);

        if state {
            self.used_map[index] |= 1usize << offset;
            self.start = self.start.min(frame);
        } else {
            self.used_map[index] &= !(1usize << offset);
            if self.start == frame {
                self.start += 1;
            }
        }
    }

    pub fn get_bit(&self, frame: usize) -> bool {
        let index = Self::frame_to_index(frame);
        let offset = Self::frame_to_offset(frame);

        (self.used_map[index] & (1usize << offset)) != 0
    }

    pub fn addr_to_pfn(addr: usize) -> usize {
        addr / PAGE_SIZE
    }

    pub fn pfn_to_addr(frame: usize) -> usize {
        frame * PAGE_SIZE
    }

    pub fn frame_to_index(frame: usize) -> usize {
        frame / BITS_PER_ENTRY
    }

    pub fn frame_to_offset(frame: usize) -> usize {
        frame % BITS_PER_ENTRY
    }
}

impl Default for BitmapMemoryTable {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapMemoryTable {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        if let Some(frame) = self.get_free_pfn() {
            self.set_frame(frame, false);
            Some(
                PhysFrame::from_start_address(PhysAddr::new(Self::pfn_to_addr(frame) as u64))
                    .unwrap(),
            )
        } else {
            None
        }
    }
}
