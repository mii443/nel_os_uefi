#![allow(non_snake_case)]

use modular_bitfield::{bitfield, prelude::B44};
use raw_cpuid::cpuid;

use crate::vmm::x86_64::intel::vcpu::IntelVCpu;
use crate::vmm::x86_64::intel::xcr0::{GUEST_XCR0_MASK, validate};

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
    __: B44,
}

pub fn supported_xcr0_mask() -> u64 {
    let supported = cpuid!(0xD, 0);
    ((supported.edx as u64) << 32) | supported.eax as u64
}

fn guest_supported_xcr0_mask() -> u64 {
    supported_xcr0_mask() & GUEST_XCR0_MASK
}

pub fn set_xcr(vcpu: &mut IntelVCpu, index: u32, xcr: u64) -> Result<(), &'static str> {
    // CPUID.0xD exposes only x87 and SSE to this guest. Keep XSETBV in
    // lock-step with that virtual capability surface even if the host supports
    // additional state components.
    validate(index, xcr, guest_supported_xcr0_mask())?;

    vcpu.guest_xcr0 = XCR0::from(xcr);

    Ok(())
}
