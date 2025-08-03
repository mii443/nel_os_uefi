use crate::cpuid::get_vendor_id;

pub fn is_amd() -> bool {
    let vendor_id = get_vendor_id();
    vendor_id == "AuthenticAMD"
}

pub fn is_intel() -> bool {
    let vendor_id = get_vendor_id();
    vendor_id == "GenuineIntel"
}
