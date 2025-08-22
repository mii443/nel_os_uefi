use modular_bitfield::{bitfield, prelude::B44};

use crate::vmm::x86_64::intel::vcpu::IntelVCpu;

#[bitfield]
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub struct XCR0 {
    pub x87: bool,
    pub sse: bool,
    pub avx: bool,
    pub bndreg: bool,
    pub bndcsr: bool,
    pub opmask: bool,
    pub zmm_hi256: bool,
    pub hi16_zmm: bool,
    pub pt: bool,
    pub pkru: bool,
    pub pasid: bool,
    pub cet_u: bool,
    pub cet_s: bool,
    pub hdc: bool,
    pub intr: bool,
    pub lbr: bool,
    pub hwp: bool,
    pub xtilecfg: bool,
    pub xtiledata: bool,
    pub apx: bool,
    #[skip]
    _reserved: B44,
}

pub fn set_xcr(vcpu: &mut IntelVCpu, index: u32, xcr: u64) -> Result<(), &'static str> {
    if index != 0 {
        return Err("Invalid XCR index");
    }

    if !(xcr & 0b1 != 0) {
        return Err("X87 is not enabled");
    }

    if (xcr & 0b100 != 0) && !(xcr & 0b10 != 0) {
        return Err("SSE is not enabled");
    }

    if !(xcr & 0b1000) != (!(xcr & 0b10000)) {
        return Err("BNDREGS and BNDCSR are not both enabled");
    }

    if xcr & 0b11100000 != 0 {
        if !(xcr & 0b100 != 0) {
            return Err("YMM bits are not enabled");
        }

        if (xcr & 0b11100000) != 0b11100000 {
            return Err("Invalid bits set in XCR0");
        }
    }

    if (xcr & 0b1000000000000 != 0) && (xcr & 0b1000000000000 != 0b1000000000000) {
        return Err("xtile bits are not both enabled");
    }

    vcpu.guest_xcr0 = XCR0::from(xcr);

    Ok(())
}
