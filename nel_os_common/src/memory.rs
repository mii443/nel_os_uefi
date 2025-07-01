#[repr(C)]
pub struct UsableMemory {
    pub ranges: *const Range,
    pub len: u64,
}

#[repr(C)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl UsableMemory {
    pub fn ranges(&self) -> &[Range] {
        unsafe { core::slice::from_raw_parts(self.ranges, self.len as usize) }
    }
}
