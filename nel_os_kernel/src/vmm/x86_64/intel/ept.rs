#![allow(non_snake_case)]

use modular_bitfield::{
    bitfield,
    prelude::{B1, B3, B4, B52},
};
use x86_64::{
    PhysAddr,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::{memory::bitmap::BitmapMemoryTable, storage::GuestMemory, vmm::x86_64::common::uefi};

pub struct Ept {
    pub root_table: PhysFrame,
}

impl Ept {
    pub fn new(allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let root_table_frame = allocator
            .allocate_frame()
            .ok_or("Failed to allocate Ept root table frame")?;

        Self::init_table(&root_table_frame);

        Ok(Self {
            root_table: root_table_frame,
        })
    }

    fn init_table(frame: &PhysFrame) {
        unsafe {
            core::ptr::write_bytes(
                frame.start_address().as_u64() as *mut u8,
                0,
                core::mem::size_of::<[EntryBase; 512]>(),
            );
        }
    }

    fn table_entry(frame: &PhysFrame) -> EntryBase {
        EntryBase::new()
            .with_read(true)
            .with_write(true)
            .with_exec_super(true)
            .with_phys(frame.start_address().as_u64() >> 12)
    }

    fn memory_entry(hpa: u64, map_memory: bool, memory_type: u8) -> EntryBase {
        EntryBase::new()
            .with_read(true)
            .with_write(true)
            .with_exec_super(true)
            .with_typ(memory_type)
            .with_map_memory(map_memory)
            .with_phys(hpa >> 12)
    }

    #[allow(dead_code)]
    pub fn map_2m(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        if gpa & 0x1f_ffff != 0 || hpa & 0x1f_ffff != 0 {
            return Err("EPT 2 MiB mapping is not aligned");
        }
        let lv4_index = (gpa >> 39) & 0x1FF;
        let lv3_index = (gpa >> 30) & 0x1FF;
        let lv2_index = (gpa >> 21) & 0x1FF;

        let lv4_table = Self::frame_to_table_ptr(&self.root_table);
        let lv4_entry = &mut lv4_table[lv4_index as usize];

        let lv3_table = if !lv4_entry.is_present() {
            let frame = allocator
                .allocate_frame()
                .ok_or("Failed to allocate LV3 frame")?;
            Self::init_table(&frame);
            let table_ptr = Self::frame_to_table_ptr(&frame);
            *lv4_entry = Self::table_entry(&frame);

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
            Self::init_table(&frame);
            let table_ptr = Self::frame_to_table_ptr(&frame);
            *lv3_entry = Self::table_entry(&frame);

            table_ptr
        } else {
            let frame = PhysFrame::from_start_address(PhysAddr::new(lv3_entry.phys() << 12))
                .map_err(|_| "Invalid LV3 frame address")?;
            Self::frame_to_table_ptr(&frame)
        };

        let lv2_entry = &mut lv2_table[lv2_index as usize];
        *lv2_entry = Self::memory_entry(hpa, true, 6);

        Ok(())
    }

    pub fn map_4k(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        self.map_4k_with_type(gpa, hpa, 6, allocator)
    }

    pub fn map_mmio_4k(
        &mut self,
        gpa: u64,
        hpa: u64,
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<(), &'static str> {
        self.map_4k_with_type(gpa, hpa, 0, allocator)
    }

    fn map_4k_with_type(
        &mut self,
        gpa: u64,
        hpa: u64,
        memory_type: u8,
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
            Self::init_table(&frame);
            let table_ptr = Self::frame_to_table_ptr(&frame);
            *lv4_entry = Self::table_entry(&frame);

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
            Self::init_table(&frame);
            let table_ptr = Self::frame_to_table_ptr(&frame);
            *lv3_entry = Self::table_entry(&frame);

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
            Self::init_table(&frame);
            let table_ptr = Self::frame_to_table_ptr(&frame);
            *lv2_entry = Self::table_entry(&frame);

            table_ptr
        } else {
            let frame = PhysFrame::from_start_address(PhysAddr::new(lv2_entry.phys() << 12))
                .map_err(|_| "Invalid LV2 frame address")?;
            Self::frame_to_table_ptr(&frame)
        };

        let lv1_entry = &mut lv1_table[lv1_index as usize];
        *lv1_entry = Self::memory_entry(hpa, true, memory_type);

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

    fn is_accessed(&self, gpa: u64) -> bool {
        let lv4_index = ((gpa >> 39) & 0x1ff) as usize;
        let lv3_index = ((gpa >> 30) & 0x1ff) as usize;
        let lv2_index = ((gpa >> 21) & 0x1ff) as usize;
        let lv1_index = ((gpa >> 12) & 0x1ff) as usize;

        let lv4_entry = &Self::frame_to_table_ptr(&self.root_table)[lv4_index];
        if !lv4_entry.is_present() {
            return false;
        }
        let Ok(frame) = PhysFrame::from_start_address(PhysAddr::new(lv4_entry.phys() << 12)) else {
            return false;
        };

        let lv3_entry = &Self::frame_to_table_ptr(&frame)[lv3_index];
        if !lv3_entry.is_present() {
            return false;
        }
        let Ok(frame) = PhysFrame::from_start_address(PhysAddr::new(lv3_entry.phys() << 12)) else {
            return false;
        };

        let lv2_entry = &Self::frame_to_table_ptr(&frame)[lv2_index];
        if !lv2_entry.is_present() {
            return false;
        }
        if lv2_entry.map_memory() {
            return lv2_entry.accessed();
        }
        let Ok(frame) = PhysFrame::from_start_address(PhysAddr::new(lv2_entry.phys() << 12)) else {
            return false;
        };

        let lv1_entry = &Self::frame_to_table_ptr(&frame)[lv1_index];
        lv1_entry.is_present() && lv1_entry.map_memory() && lv1_entry.accessed()
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
        let lv4_table = Self::frame_to_table_ptr(&self.root_table);
        for (lv4_index, lv4_entry) in lv4_table.iter().copied().enumerate() {
            if !lv4_entry.is_present() || lv4_entry.map_memory() {
                continue;
            }
            let Some(lv3_frame) = Self::entry_frame(lv4_entry) else {
                continue;
            };
            let lv3_table = Self::frame_to_table_ptr(&lv3_frame);
            for (lv3_index, lv3_entry) in lv3_table.iter().copied().enumerate() {
                if !lv3_entry.is_present() || lv3_entry.map_memory() {
                    continue;
                }
                let Some(lv2_frame) = Self::entry_frame(lv3_entry) else {
                    continue;
                };
                let lv2_table = Self::frame_to_table_ptr(&lv2_frame);
                for (lv2_index, lv2_entry) in lv2_table.iter().copied().enumerate() {
                    if !lv2_entry.is_present() {
                        continue;
                    }
                    let gpa_2m = (lv4_index as u64) << 39
                        | (lv3_index as u64) << 30
                        | (lv2_index as u64) << 21;
                    if lv2_entry.map_memory() {
                        if uefi::ram_range_containing(guest_memory_size, gpa_2m).is_some()
                            && let Some(frame) = Self::entry_frame(lv2_entry)
                        {
                            allocator.deallocate_contiguous_frames(frame, 512);
                        }
                        continue;
                    }
                    let Some(lv1_frame) = Self::entry_frame(lv2_entry) else {
                        continue;
                    };
                    let lv1_table = Self::frame_to_table_ptr(&lv1_frame);
                    for (lv1_index, lv1_entry) in lv1_table.iter().copied().enumerate() {
                        let gpa = gpa_2m | (lv1_index as u64) << 12;
                        if lv1_entry.is_present()
                            && lv1_entry.map_memory()
                            && uefi::owns_backing_page(guest_memory_size, gpa)
                            && let Some(frame) = Self::entry_frame(lv1_entry)
                        {
                            allocator.deallocate_frame(frame);
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

    fn entry_frame(entry: EntryBase) -> Option<PhysFrame<Size4KiB>> {
        PhysFrame::from_start_address(PhysAddr::new(entry.phys() << 12)).ok()
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
            let hpa = self
                .get_phys_addr(gpa)
                .ok_or("Failed to get physical address")?;
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
                .ok_or("Failed to get physical address")?;
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
                .get_phys_addr(gpa)
                .ok_or("Failed to get physical address")?;
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

    fn frame_to_table_ptr(frame: &PhysFrame) -> &'static mut [EntryBase; 512] {
        let table_ptr = frame.start_address().as_u64();

        unsafe { &mut *(table_ptr as *mut [EntryBase; 512]) }
    }
}

impl GuestMemory for Ept {
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

#[bitfield]
#[repr(u64)]
#[derive(Debug)]
pub struct Eptp {
    pub typ: B3,
    pub level: B3,
    pub dirty_accessed: bool,
    pub enforce_access_rights: bool,
    _reserved: B4,
    pub phys: B52,
}

impl Eptp {
    pub fn init(lv4_table: &PhysFrame) -> Self {
        Eptp::new()
            .with_typ(6)
            .with_level(3)
            .with_dirty_accessed(true)
            .with_enforce_access_rights(false)
            .with_phys(lv4_table.start_address().as_u64() >> 12)
    }

    pub fn get_lv4_table(&mut self) -> &mut [EntryBase; 512] {
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
