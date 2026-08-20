use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::{Cr3, Cr3Flags},
    structures::paging::{
        FrameAllocator, PageSize, PageTable, PageTableFlags, PhysFrame, Size1GiB, Size4KiB,
        page_table::FrameError,
    },
};

use crate::info;

const PML4_ENTRY_BYTES: u64 = 512 * Size1GiB::SIZE;
const LOWER_HALF_PML4_ENTRIES: usize = 256;

pub fn init_page_table(
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    physical_end: u64,
) -> Result<*mut PageTable, &'static str> {
    if physical_end == 0 {
        return Err("physical memory map is empty");
    }
    let required_entries = physical_end.div_ceil(PML4_ENTRY_BYTES) as usize;
    if required_entries > LOWER_HALF_PML4_ENTRIES {
        return Err("physical memory exceeds the 4-level direct-map address space");
    }

    let (lv4_frame, lv4_table) = new_page_table(frame_allocator);

    let base_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::ACCESSED;

    let lv4: &mut PageTable = unsafe { &mut *lv4_table };
    for lv4_index in 0..required_entries {
        let (lv3_frame, lv3_table) = new_page_table(frame_allocator);
        lv4[lv4_index].set_frame(lv3_frame, base_flags);
        let lv3: &mut PageTable = unsafe { &mut *lv3_table };
        for (lv3_index, lv3_pte) in lv3.iter_mut().enumerate() {
            let address = lv4_index as u64 * PML4_ENTRY_BYTES + lv3_index as u64 * Size1GiB::SIZE;
            lv3_pte.set_addr(
                PhysAddr::new(address),
                base_flags | PageTableFlags::HUGE_PAGE,
            );
        }
    }

    info!("Setting new page table...");

    unsafe {
        Cr3::write(lv4_frame, Cr3Flags::empty());
    }

    Ok(lv4_table)
}

fn new_page_table(
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> (PhysFrame, *mut PageTable) {
    let frame = frame_allocator.allocate_frame().unwrap();
    let address = frame.start_address().as_u64();
    unsafe {
        core::ptr::write_bytes(address as *mut u8, 0, Size4KiB::SIZE as usize);
    }

    (frame, VirtAddr::new(address).as_mut_ptr())
}

pub fn get_active_level_4_table() -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();

    frame_to_page_table(level_4_table_frame)
}

pub fn frame_to_page_table(frame: PhysFrame) -> &'static mut PageTable {
    let page_table_addr = frame.start_address().as_u64();
    let page_table_ptr: *mut PageTable = VirtAddr::new(page_table_addr).as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

pub fn translate_addr(addr: VirtAddr) -> Option<PhysAddr> {
    let (level_4_table_frame, _) = Cr3::read();

    let table_indexes = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];

    let mut frame = level_4_table_frame;

    let table = frame_to_page_table(frame);
    let entry = &table[table_indexes[0]];
    frame = match entry.frame() {
        Ok(frame) => frame,
        Err(FrameError::FrameNotPresent) => return None,
        Err(FrameError::HugeFrame) => panic!("1GiB pages at level 4 are not supported"),
    };

    let table = frame_to_page_table(frame);
    let entry = &table[table_indexes[1]];
    match entry.frame() {
        Ok(frame_4k) => {
            frame = frame_4k;
        }
        Err(FrameError::FrameNotPresent) => return None,
        Err(FrameError::HugeFrame) => {
            let huge_frame_addr = entry.addr();
            let offset_1gib = addr.as_u64() & 0x3FFF_FFFF;
            return Some(huge_frame_addr + offset_1gib);
        }
    };

    let table = frame_to_page_table(frame);
    let entry = &table[table_indexes[2]];
    match entry.frame() {
        Ok(frame_4k) => {
            frame = frame_4k;
        }
        Err(FrameError::FrameNotPresent) => return None,
        Err(FrameError::HugeFrame) => {
            let huge_frame_addr = entry.addr();
            let offset_2mib = addr.as_u64() & 0x1F_FFFF;
            return Some(huge_frame_addr + offset_2mib);
        }
    };

    let table = frame_to_page_table(frame);
    let entry = &table[table_indexes[3]];
    frame = match entry.frame() {
        Ok(frame) => frame,
        Err(FrameError::FrameNotPresent) => return None,
        Err(FrameError::HugeFrame) => panic!("Huge pages at level 1 are not supported"),
    };

    Some(frame.start_address() + u64::from(addr.page_offset()))
}
