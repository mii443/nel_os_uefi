use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

const PAGE_SIZE: u64 = 4096;
const IOPM_PAGES: usize = 3;
const MSRPM_PAGES: usize = 2;

/// The SVM permission maps must occupy physically contiguous pages.  The
/// generic allocator only hands out one frame at a time, so keep allocating
/// until it returns a suitably long run.  Frames preceding the run remain
/// reserved: the allocator has no deallocation operation, and reusing them
/// behind its back would be unsafe.
fn allocate_contiguous(
    frame_allocator: &mut dyn FrameAllocator<Size4KiB>,
    pages: usize,
) -> Result<PhysFrame, &'static str> {
    let mut run_start = None;
    let mut run_len = 0usize;

    while let Some(frame) = frame_allocator.allocate_frame() {
        let address = frame.start_address().as_u64();
        match run_start {
            Some(start) if address == start + run_len as u64 * PAGE_SIZE => run_len += 1,
            _ => {
                run_start = Some(address);
                run_len = 1;
            }
        }

        if run_len == pages {
            return Ok(PhysFrame::containing_address(x86_64::PhysAddr::new(
                run_start.unwrap(),
            )));
        }
    }

    Err("No contiguous frames for AMD SVM permission map")
}

pub struct PermissionMaps {
    iopm: PhysFrame,
    msrpm: PhysFrame,
}

impl PermissionMaps {
    pub fn new(frame_allocator: &mut dyn FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let iopm = allocate_contiguous(frame_allocator, IOPM_PAGES)?;
        let msrpm = allocate_contiguous(frame_allocator, MSRPM_PAGES)?;

        // A set bit requests interception.  Default-deny every I/O port and
        // every MSR covered by the architectural MSRPM ranges.
        unsafe {
            core::ptr::write_bytes(
                iopm.start_address().as_u64() as *mut u8,
                u8::MAX,
                IOPM_PAGES * PAGE_SIZE as usize,
            );
            core::ptr::write_bytes(
                msrpm.start_address().as_u64() as *mut u8,
                u8::MAX,
                MSRPM_PAGES * PAGE_SIZE as usize,
            );
        }

        Ok(Self { iopm, msrpm })
    }

    pub fn iopm_base_pa(&self) -> u64 {
        self.iopm.start_address().as_u64()
    }

    pub fn msrpm_base_pa(&self) -> u64 {
        self.msrpm.start_address().as_u64()
    }
}
