use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

const PAGE_SIZE: u64 = 4096;
const IOPM_PAGES: usize = 3;
const MSRPM_PAGES: usize = 2;
const MSRPM_SIZE: usize = MSRPM_PAGES * PAGE_SIZE as usize;

const GUEST_STATE_MSRS: &[u32] = &[
    0x0000_0174, // SYSENTER_CS
    0x0000_0175, // SYSENTER_ESP
    0x0000_0176, // SYSENTER_EIP
    0x0000_0277, // PAT
    0xc000_0080, // EFER
    0xc000_0081, // STAR
    0xc000_0082, // LSTAR
    0xc000_0083, // CSTAR
    0xc000_0084, // SFMASK
    0xc000_0100, // FS_BASE
    0xc000_0101, // GS_BASE
    0xc000_0102, // KERNEL_GS_BASE
];

/// Return the byte and first bit for an MSR's read/write intercept pair.
///
/// AMD divides the 8 KiB MSRPM into three 2 KiB ranges. Each MSR consumes
/// two adjacent bits: read first, then write. The final 2 KiB is reserved.
fn msrpm_location(msr: u32) -> Option<(usize, u8)> {
    let (range_base, index) = match msr {
        0x0000_0000..=0x0000_1fff => (0x0000, msr),
        0xc000_0000..=0xc000_1fff => (0x0800, msr - 0xc000_0000),
        0xc001_0000..=0xc001_1fff => (0x1000, msr - 0xc001_0000),
        _ => return None,
    };
    Some((range_base + index as usize / 4, ((index & 3) * 2) as u8))
}

fn allow_guest_state_msr(msrpm: &mut [u8], msr: u32) -> Result<(), &'static str> {
    let (byte, bit) = msrpm_location(msr).ok_or("MSR is outside the AMD MSRPM ranges")?;
    msrpm[byte] &= !(0b11 << bit);
    Ok(())
}

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

        let msrpm_bytes = unsafe {
            core::slice::from_raw_parts_mut(msrpm.start_address().as_u64() as *mut u8, MSRPM_SIZE)
        };
        for &msr in GUEST_STATE_MSRS {
            allow_guest_state_msr(msrpm_bytes, msr)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_architectural_msrpm_ranges() {
        assert_eq!(msrpm_location(0), Some((0, 0)));
        assert_eq!(msrpm_location(3), Some((0, 6)));
        assert_eq!(msrpm_location(0x1fff), Some((0x07ff, 6)));
        assert_eq!(msrpm_location(0xc000_0000), Some((0x0800, 0)));
        assert_eq!(msrpm_location(0xc000_1fff), Some((0x0fff, 6)));
        assert_eq!(msrpm_location(0xc001_0000), Some((0x1000, 0)));
        assert_eq!(msrpm_location(0xc001_1fff), Some((0x17ff, 6)));
        assert_eq!(msrpm_location(0xc002_0000), None);
    }

    #[test]
    fn only_clears_selected_read_write_pair() {
        let mut map = [u8::MAX; MSRPM_SIZE];
        allow_guest_state_msr(&mut map, 0x277).unwrap();
        assert_eq!(map[0x09d], 0x3f);
        assert_eq!(map[0x09c], u8::MAX);
        assert_eq!(map[0x09e], u8::MAX);

        allow_guest_state_msr(&mut map, 0xc000_0080).unwrap();
        assert_eq!(map[0x0820], 0xfc);
    }
}
