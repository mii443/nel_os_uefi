use x86_64::{
    registers::control::Cr3,
    structures::paging::{
        page_table::FrameError, FrameAllocator, PageSize, PageTable, PageTableFlags, PhysFrame,
        Size1GiB, Size4KiB,
    },
    PhysAddr, VirtAddr,
};

pub fn init_page_table(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> &mut PageTable {
    let (_, lv4_table) = new_page_table(frame_allocator);
    let (lv3_frame, lv3_table) = new_page_table(frame_allocator);

    let base_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::GLOBAL;

    let lv4: &mut PageTable = unsafe { &mut *lv4_table };

    lv4[0].set_frame(lv3_frame, base_flags);

    unsafe {
        let lv3: &mut PageTable = &mut *lv3_table;

        for (index, lv3_pte) in lv3.iter_mut().enumerate() {
            lv3_pte.set_addr(
                PhysAddr::new(index as u64 * Size1GiB::SIZE),
                base_flags | PageTableFlags::HUGE_PAGE,
            );
        }
    }

    lv4
}

fn new_page_table(
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> (PhysFrame, *mut PageTable) {
    let frame = frame_allocator.allocate_frame().unwrap();

    (
        frame,
        VirtAddr::new(frame.start_address().as_u64()).as_mut_ptr(),
    )
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
