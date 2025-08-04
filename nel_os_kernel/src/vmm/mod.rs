use alloc::boxed::Box;

use crate::{
    platform,
    vmm::x86_64::{amd::AMDVCpu, intel::IntelVCpu},
};

pub mod x86_64;

pub trait VCpu {
    fn is_supported() -> bool
    where
        Self: Sized;
    fn run(&mut self);
}

pub fn get_vcpu() -> Box<dyn VCpu> {
    if platform::is_amd() && AMDVCpu::is_supported() {
        Box::new(AMDVCpu::new())
    } else if platform::is_intel() && IntelVCpu::is_supported() {
        Box::new(IntelVCpu::new())
    } else {
        panic!("Unsupported CPU architecture");
    }
}
