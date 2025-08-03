use alloc::boxed::Box;

use crate::{
    platform,
    vmm::x86_64::{amd::AMDVCpu, intel::IntelVCpu},
};

pub mod x86_64;

pub trait VCpu {
    fn run(&mut self);
}

pub fn get_vcpu() -> Box<dyn VCpu> {
    if platform::is_amd() {
        Box::new(AMDVCpu::new())
    } else if platform::is_intel() {
        Box::new(IntelVCpu::new())
    } else {
        panic!("Unsupported CPU architecture");
    }
}
