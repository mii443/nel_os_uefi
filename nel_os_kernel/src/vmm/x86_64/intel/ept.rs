use modular_bitfield::{
    bitfield,
    prelude::{B3, B53},
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

        Self::init_table(&root_table_frame);

        Ok(Self {
            root_table: root_table_frame,
        })
    }

    fn init_table(frame: &PhysFrame) {
        let table_ptr = frame.start_address().as_u64();
        let entries = unsafe { &mut *(table_ptr as *mut [EntryBase; 512]) };

        for entry in entries {
            entry.set_read(false);
            entry.set_write(false);
            entry.set_exec_super(false);
            entry.set_map_memory(false);
            entry.set_typ(0);
        }
    }

    pub fn map_2m(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut impl FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        let lv4_index = (gpa >> 39) & 0x1FF;
        let lv3_index = (gpa >> 30) & 0x1FF;
        let lv2_index = (gpa >> 21) & 0x1FF;

        let lv4_table = Self::frame_to_table_ptr(&self.root_table);
        let lv4_entry = &mut lv4_table[lv4_index as usize];

        let lv3_table = if !lv4_entry.is_present() {
            let frame = allocator
                .allocate_frame()
                .ok_or("Failed to allocate LV3 frame")?;
            let table_ptr = Self::frame_to_table_ptr(&frame);
            lv4_entry.set_phys(frame.start_address().as_u64() >> 12);
            lv4_entry.set_map_memory(false);
            lv4_entry.set_typ(0);
            lv4_entry.set_read(true);
            lv4_entry.set_write(true);
            lv4_entry.set_exec_super(true);

            table_ptr
        } else {
            let frame = PhysFrame::from_start_address(PhysAddr::new(lv4_entry.phys() << 12))
                .map_err(|_| "Invalid LV4 frame address")?;
            Self::frame_to_table_ptr(&frame)
        };

        let lv3_entry = &mut lv3_table[lv3_index as usize];

        let lv2_table = if !lv3_entry.is_present() {
            let frame = allocator
                .allocate_frame()
                .ok_or("Failed to allocate LV2 frame")?;
            let table_ptr = Self::frame_to_table_ptr(&frame);
            lv3_entry.set_phys(frame.start_address().as_u64() >> 12);
            lv3_entry.set_map_memory(false);
            lv3_entry.set_typ(0);
            lv3_entry.set_read(true);
            lv3_entry.set_write(true);
            lv3_entry.set_exec_super(true);

            table_ptr
        } else {
            let frame = PhysFrame::from_start_address(PhysAddr::new(lv3_entry.phys() << 12))
                .map_err(|_| "Invalid LV3 frame address")?;
            Self::frame_to_table_ptr(&frame)
        };

        let lv2_entry = &mut lv2_table[lv2_index as usize];
        lv2_entry.set_phys(hpa >> 12);
        lv2_entry.set_map_memory(true);
        lv2_entry.set_typ(0);
        lv2_entry.set_read(true);
        lv2_entry.set_write(true);
        lv2_entry.set_exec_super(true);

        Ok(())
    }

    fn frame_to_table_ptr(frame: &PhysFrame) -> &'static mut [EntryBase; 512] {
        let table_ptr = frame.start_address().as_u64();

        unsafe { &mut *(table_ptr as *mut [EntryBase; 512]) }
    }
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
