use modular_bitfield::{
    bitfield,
    prelude::{B3, B53},
    Specifier,
};
use x86_64::{
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
    PhysAddr,
};

pub struct EPT {
    pub root_table: PhysFrame,
}

impl EPT {
    pub fn new(allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let root_table_frame = allocator
            .allocate_frame()
            .ok_or("Failed to allocate EPT root table frame")?;

        Ok(Self {
            root_table: root_table_frame,
        })
    }

    fn init_table(frame: &PhysFrame) {}
}

#[bitfield]
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub struct EntryBase {
    pub read: bool,
    pub write: bool,
    pub exec_super: bool,
    pub typ: B3,
    pub ignore_pat: bool,
    pub map_memory: bool,
    pub accessed: bool,
    pub dirty: bool,
    pub exec_user: bool,
    pub phys: B53,
}

impl EntryBase {
    pub fn is_present(&self) -> bool {
        self.read() || self.write() || self.exec_super()
    }

    pub fn address(&self) -> PhysAddr {
        PhysAddr::new(self.phys() << 12)
    }
}

impl Default for EntryBase {
    fn default() -> Self {
        Self::new()
            .with_read(true)
            .with_write(true)
            .with_exec_super(true)
            .with_typ(0)
            .with_ignore_pat(false)
            .with_map_memory(false)
            .with_accessed(false)
            .with_dirty(false)
            .with_exec_user(true)
            .with_phys(0)
    }
}
