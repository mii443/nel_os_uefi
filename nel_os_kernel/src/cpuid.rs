use alloc::string::{String, ToString};
use raw_cpuid::CpuId;

pub fn get_vendor_id() -> String {
    let cpuid = CpuId::new();
    if let Some(vf) = cpuid.get_vendor_info() {
        return vf.as_str().to_string();
    }
    "Unknown".to_string()
}

pub fn get_brand() -> String {
    let cpuid = CpuId::new();
    if let Some(brand) = cpuid.get_processor_brand_string() {
        return brand.as_str().to_string();
    }
    "Unknown".to_string()
}
