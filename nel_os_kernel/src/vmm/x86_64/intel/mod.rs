use crate::{info, vmm::VCpu};

pub struct IntelVCpu;

impl IntelVCpu {
    pub fn new() -> Self {
        IntelVCpu
    }
}

impl VCpu for IntelVCpu {
    fn run(&mut self) {
        info!("VCpu on Intel");
    }
}
