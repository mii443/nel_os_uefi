use nel_os_common::memory::{self, UsableMemory};

use crate::constant::ENTRY_COUNT;

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
}
