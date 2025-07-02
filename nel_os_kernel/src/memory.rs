use nel_os_common::memory::{self, UsableMemory};

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

    pub fn init(usable_memory: UsableMemory) -> Self {
        let table = Self::new();
        for range in usable_memory.ranges() {}

        table
    }

    pub fn set_range(&mut self, range: memory::Range) {
        let start = range.start;
        let end = range.end;
    }

    pub fn set_frame(frame: usize, state: bool) {}

    pub fn frame_to_index(frame: usize) -> usize {
        frame / BITS_PER_ENTRY
    }

    pub fn frame_to_offset(frame: usize) -> usize {
        frame % BITS_PER_ENTRY
    }
}
