pub mod linux;
pub mod segment;

use core::arch::asm;

pub trait X86VCpu {
    fn set_segment_rights(&mut self, segment: segment::Segment, rights: segment::SegmentRights);
    fn set_segment_base(&mut self, segment: segment::Segment, base: u64);
    fn set_segment_limit(&mut self, segment: segment::Segment, limit: u32);
    fn set_segment_selector(&mut self, segment: segment::Segment, selector: u16);
}

pub fn read_msr(msr: u32) -> u64 {
    let mut low: u32;
    let mut high: u32;

    unsafe {
        asm!(
            "rdmsr",
            out("eax") low,
            out("edx") high,
            in("ecx") msr,
        );
    }

    ((high as u64) << 32) | (low as u64)
}

pub fn write_msr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;

    unsafe {
        asm!(
            "wrmsr",
            in("eax") low,
            in("edx") high,
            in("ecx") msr,
        );
    }
}
