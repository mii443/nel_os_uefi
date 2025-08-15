#![allow(non_snake_case)]

use modular_bitfield::{
    bitfield,
    prelude::{B1, B3, B4, B52},
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

    #[allow(dead_code)]
    pub fn map_2m(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
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

    pub fn map_4k(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        let lv4_index = (gpa >> 39) & 0x1FF;
        let lv3_index = (gpa >> 30) & 0x1FF;
        let lv2_index = (gpa >> 21) & 0x1FF;
        let lv1_index = (gpa >> 12) & 0x1FF;

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

        let lv1_table = if !lv2_entry.is_present() || lv2_entry.map_memory() {
            let frame = allocator
                .allocate_frame()
                .ok_or("Failed to allocate LV1 frame")?;
            let table_ptr = Self::frame_to_table_ptr(&frame);
            lv2_entry.set_phys(frame.start_address().as_u64() >> 12);
            lv2_entry.set_map_memory(false);
            lv2_entry.set_typ(0);
            lv2_entry.set_read(true);
            lv2_entry.set_write(true);
            lv2_entry.set_exec_super(true);

            table_ptr
        } else {
            let frame = PhysFrame::from_start_address(PhysAddr::new(lv2_entry.phys() << 12))
                .map_err(|_| "Invalid LV2 frame address")?;
            Self::frame_to_table_ptr(&frame)
        };

        let lv1_entry = &mut lv1_table[lv1_index as usize];
        lv1_entry.set_phys(hpa >> 12);
        lv1_entry.set_map_memory(true);
        lv1_entry.set_typ(0);
        lv1_entry.set_read(true);
        lv1_entry.set_write(true);
        lv1_entry.set_exec_super(true);

        Ok(())
    }

    pub fn get_phys_addr(&self, gpa: u64) -> Option<u64> {
        let lv4_index = (gpa >> 39) & 0x1FF;
        let lv3_index = (gpa >> 30) & 0x1FF;
        let lv2_index = (gpa >> 21) & 0x1FF;
        let lv1_index = (gpa >> 12) & 0x1FF;

        let lv4_table = Self::frame_to_table_ptr(&self.root_table);
        let lv4_entry = &lv4_table[lv4_index as usize];

        if !lv4_entry.is_present() {
            return None;
        }

        let frame = PhysFrame::from_start_address(PhysAddr::new(lv4_entry.phys() << 12)).ok()?;
        let lv3_table = Self::frame_to_table_ptr(&frame);
        let lv3_entry = &lv3_table[lv3_index as usize];

        if !lv3_entry.is_present() {
            return None;
        }

        let frame = PhysFrame::from_start_address(PhysAddr::new(lv3_entry.phys() << 12)).ok()?;
        let lv2_table = Self::frame_to_table_ptr(&frame);
        let lv2_entry = &lv2_table[lv2_index as usize];

        if !lv2_entry.is_present() {
            return None;
        }

        if lv2_entry.map_memory() {
            let page_offset = gpa & 0x1FFFFF;
            let phys_addr_base = lv2_entry.address().as_u64();
            Some(phys_addr_base | page_offset)
        } else {
            let frame =
                PhysFrame::from_start_address(PhysAddr::new(lv2_entry.phys() << 12)).ok()?;
            let lv1_table = Self::frame_to_table_ptr(&frame);
            let lv1_entry = &lv1_table[lv1_index as usize];

            if !lv1_entry.is_present() || !lv1_entry.map_memory() {
                return None;
            }

            let page_offset = gpa & 0xFFF;
            let phys_addr_base = lv1_entry.address().as_u64();
            Some(phys_addr_base | page_offset)
        }
    }

    pub fn get(&mut self, gpa: u64) -> Result<u8, &'static str> {
        let hpa = self
            .get_phys_addr(gpa)
            .ok_or("Failed to get physical address")?;

        let guest_memory = unsafe { &*(hpa as *const u8) };

        Ok(*guest_memory)
    }

    pub fn set(&mut self, gpa: u64, value: u8) -> Result<(), &'static str> {
        let hpa = self
            .get_phys_addr(gpa)
            .ok_or("Failed to get physical address")?;

        let guest_memory = unsafe { &mut *(hpa as *mut u8) };
        *guest_memory = value;

        Ok(())
    }

    pub fn set_range(
        &mut self,
        gpa_start: u64,
        gpa_end: u64,
        value: u8,
    ) -> Result<(), &'static str> {
        if gpa_start > gpa_end {
            return Err("Invalid GPA range");
        }

        let mut gpa = gpa_start;
        while gpa < gpa_end {
            self.set(gpa, value)?;
            gpa += 1;
        }

        Ok(())
    }

    fn frame_to_table_ptr(frame: &PhysFrame) -> &'static mut [EntryBase; 512] {
        let table_ptr = frame.start_address().as_u64();

        unsafe { &mut *(table_ptr as *mut [EntryBase; 512]) }
    }
}

#[bitfield]
#[repr(u64)]
#[derive(Debug)]
pub struct EPTP {
    pub typ: B3,
    pub level: B3,
    pub dirty_accessed: bool,
    pub enforce_access_rights: bool,
    _reserved: B4,
    pub phys: B52,
}

impl EPTP {
    pub fn init(lv4_table: &PhysFrame) -> Self {
        EPTP::new()
            .with_typ(6)
            .with_level(3)
            .with_dirty_accessed(true)
            .with_enforce_access_rights(false)
            .with_phys(lv4_table.start_address().as_u64() >> 12)
    }

    pub fn get_lv4_table(&self) -> &mut [EntryBase; 512] {
        let table_ptr = self.phys() << 12;

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
    _reserved: B1,
    pub phys: B52,
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
