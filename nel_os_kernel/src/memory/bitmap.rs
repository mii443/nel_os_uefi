use core::slice;

use nel_os_common::memory::{self, UsableMemory};
use spin::Once;
use x86_64::{
    PhysAddr,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::constant::{BITS_PER_ENTRY, PAGE_SIZE};

pub static MAX_MEMORY: Once<usize> = Once::new();

pub fn get_entry_count() -> usize {
    MAX_MEMORY.get().unwrap_or(&0) / PAGE_SIZE / BITS_PER_ENTRY
}

pub struct BitmapMemoryTable {
    pub used_map: &'static mut [usize],
    pub start: usize,
    pub end: usize,
    free_frames: usize,
}

impl BitmapMemoryTable {
    pub fn init(usable_memory: &UsableMemory) -> Self {
        let mut max_addr = 0u64;
        for range in usable_memory.ranges() {
            max_addr = max_addr.max(range.end);
        }

        let entry_count = get_entry_count();
        let bitmap_size = entry_count * core::mem::size_of::<usize>();

        let bitmap_addr = ((max_addr as usize).saturating_sub(bitmap_size)) & !(PAGE_SIZE - 1);

        let used_map = unsafe {
            let ptr = bitmap_addr as *mut usize;
            slice::from_raw_parts_mut(ptr, entry_count)
        };

        (0..entry_count).for_each(|i| {
            used_map[i] = 0;
        });

        let mut table = Self {
            used_map,
            start: 0,
            end: usize::MAX,
            free_frames: 0,
        };

        for range in usable_memory.ranges() {
            table.set_range(range);
        }

        // Never hand out physical page zero. Besides catching null pointers,
        // firmware memory maps are allowed to describe boot-services storage
        // at address zero, while Rust references may never be null.
        table.set_frame(0, false);

        let bitmap_start_frame = Self::addr_to_pfn(bitmap_addr);
        let bitmap_frames = bitmap_size.div_ceil(PAGE_SIZE);
        for i in 0..bitmap_frames {
            table.set_frame(bitmap_start_frame + i, false);
        }

        for i in 0..entry_count {
            let index = entry_count - i - 1;
            if table.used_map[index] != 0 {
                let offset = 63 - table.used_map[index].leading_zeros();
                table.end = index * BITS_PER_ENTRY + (BITS_PER_ENTRY - offset as usize);
                break;
            }
        }

        table
    }

    pub fn get_free_pfn(&self) -> Option<usize> {
        (self.start..self.end).find(|&i| self.get_bit(i))
    }

    pub fn free_frame_count(&self) -> usize {
        self.free_frames
    }

    /// Allocates a physically contiguous, PFN-aligned run of frames.
    pub fn allocate_contiguous_frames(
        &mut self,
        count: usize,
        alignment_frames: usize,
    ) -> Option<PhysFrame<Size4KiB>> {
        if count == 0 || !alignment_frames.is_power_of_two() {
            return None;
        }

        let alignment_mask = alignment_frames - 1;
        let mut candidate = self.start.checked_add(alignment_mask)? & !alignment_mask;
        while candidate.checked_add(count)? <= self.end {
            let mut offset = 0;
            while offset < count && self.get_bit(candidate + offset) {
                offset += 1;
            }
            if offset == count {
                for frame in candidate..candidate + count {
                    self.set_frame(frame, false);
                }
                return PhysFrame::from_start_address(PhysAddr::new(
                    Self::pfn_to_addr(candidate) as u64
                ))
                .ok();
            }
            candidate = candidate.checked_add(offset + 1 + alignment_mask)? & !alignment_mask;
        }
        None
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
        let was_free = (self.used_map[index] & (1usize << offset)) != 0;

        if was_free == state {
            return;
        }

        if state {
            self.used_map[index] |= 1usize << offset;
            self.start = self.start.min(frame);
            self.free_frames += 1;
        } else {
            self.used_map[index] &= !(1usize << offset);
            self.free_frames = self.free_frames.saturating_sub(1);
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

unsafe impl FrameAllocator<Size4KiB> for BitmapMemoryTable {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        if let Some(frame) = self.get_free_pfn() {
            // `get_free_pfn` found the first free frame at or after `start`, so
            // everything before it is allocated.  Advancing the cursor avoids
            // repeatedly rescanning a large aligned allocation.
            self.start = frame;
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
