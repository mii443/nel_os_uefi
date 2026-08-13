use core::arch::x86_64::{_xgetbv, _xsave64, _xsetbv};

use raw_cpuid::cpuid;
use x86_64::{
    registers::control::{Cr4, Cr4Flags},
    structures::paging::{FrameAllocator, Size4KiB},
};

const PAGE_SIZE: usize = 4096;

const fn pages_required(size: usize) -> usize {
    size.div_ceil(PAGE_SIZE)
}

/// Host extended processor state which is not covered by FXSAVE.
///
/// The backing frames are deliberately retained for the lifetime of the vCPU.
/// Page alignment exceeds XSAVE's 64-byte alignment requirement.
pub struct HostXsaveState {
    addr: u64,
    mask: u64,
    size: usize,
}

impl HostXsaveState {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let features = cpuid!(1, 0);
        if features.ecx & (1 << 26) == 0 {
            return Ok(Self {
                addr: 0,
                mask: 0,
                size: 0,
            });
        }

        // The VMM itself owns extended-state switching. Firmware may leave
        // OSXSAVE clear even on an XSAVE-capable CPU, so enable it before the
        // first XGETBV/XSAVE and retain it as a host invariant.
        unsafe { Cr4::update(|flags| flags.insert(Cr4Flags::OSXSAVE)) };

        let supported = cpuid!(0xD, 0);
        let supported_mask = ((supported.edx as u64) << 32) | supported.eax as u64;
        let mut mask = unsafe { _xgetbv(0) };
        // x86-64 hosts may have used legacy SSE before OSXSAVE was enabled.
        // Make x87+SSE explicit XSAVE components so XMM0-15 are included in
        // the host image restored after every guest exit.
        if supported_mask & 3 == 3 && mask & 3 != 3 {
            mask |= 3;
            unsafe { _xsetbv(0, mask) };
        }
        let size = cpuid!(0xD, 0).ebx as usize;
        if mask == 0 || size < 512 {
            return Err("Host reported an invalid XSAVE configuration");
        }

        let pages = pages_required(size);
        let first = frame_allocator
            .allocate_frame()
            .ok_or("Failed to allocate host XSAVE area")?;
        let addr = first.start_address().as_u64();

        for page in 1..pages {
            let frame = frame_allocator
                .allocate_frame()
                .ok_or("Failed to allocate contiguous host XSAVE area")?;
            if frame.start_address().as_u64() != addr + (page * PAGE_SIZE) as u64 {
                return Err("Host XSAVE area frames are not contiguous");
            }
        }

        unsafe { core::ptr::write_bytes(addr as *mut u8, 0, pages * PAGE_SIZE) };

        Ok(Self { addr, mask, size })
    }

    pub fn is_enabled(&self) -> bool {
        self.mask != 0
    }

    pub fn addr(&self) -> u64 {
        self.addr
    }

    pub fn mask(&self) -> u64 {
        self.mask
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Save every state component enabled in the host XCR0, then narrow XCR0
    /// for the guest. VM-exit assembly must restore both XCR0 and this image
    /// before returning to Rust.
    pub unsafe fn save_host_and_load_guest(&self, guest_xcr0: u64) -> Result<(), &'static str> {
        if !self.is_enabled() {
            return Ok(());
        }
        if guest_xcr0 == 0 || guest_xcr0 & !self.mask != 0 {
            return Err("Invalid guest XCR0 for this host");
        }

        unsafe {
            _xsave64(self.addr as *mut u8, self.mask);
            _xsetbv(0, guest_xcr0);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_covers_xsave_area() {
        assert_eq!(pages_required(512), 1);
        assert_eq!(pages_required(4096), 1);
        assert_eq!(pages_required(4097), 2);
    }
}
