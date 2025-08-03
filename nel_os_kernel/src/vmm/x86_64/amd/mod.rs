use crate::{info, vmm::VCpu};

pub struct AMDVCpu;

impl AMDVCpu {
    pub fn new() -> Self {
        AMDVCpu
    }
}

impl VCpu for AMDVCpu {
    fn run(&mut self) {
        info!("VCpu on AMD");
    }
}
