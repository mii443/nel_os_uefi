//! CPUID leaves shared by the VMX and SVM backends.

/// The first hypervisor-specific leaf implemented by nel_os.
pub const HYPERVISOR_BASE_LEAF: u32 = 0x4000_0000;
pub const HYPERVISOR_FREQUENCY_LEAF: u32 = HYPERVISOR_BASE_LEAF + 0x10;

/// Return the standard hypervisor vendor leaf.
///
/// CPUID places the twelve-byte vendor ID in EBX, ECX, EDX order.  Keep the
/// The frequency leaf follows the de-facto KVM CPUID contract understood by
/// FreeBSD and lets guests avoid slow statistical timer calibration.
pub const fn hypervisor_vendor_leaf() -> (u32, u32, u32, u32) {
    (
        HYPERVISOR_FREQUENCY_LEAF,
        u32::from_le_bytes(*b"NEL "),
        u32::from_le_bytes(*b"NEL "),
        u32::from_le_bytes(*b"NEL "),
    )
}

/// Return virtual TSC and local-APIC frequencies in kHz.
pub const fn hypervisor_frequency_leaf(tsc_khz: u32) -> (u32, u32, u32, u32) {
    // The emulated local APIC counter is driven directly from the same TSC
    // clock, before applying the guest-programmed APIC divisor.
    (tsc_khz, tsc_khz, 0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_leaf_has_a_twelve_byte_signature() {
        let (max_leaf, ebx, ecx, edx) = hypervisor_vendor_leaf();

        assert_eq!(max_leaf, HYPERVISOR_FREQUENCY_LEAF);
        assert_eq!(
            [ebx.to_le_bytes(), ecx.to_le_bytes(), edx.to_le_bytes()].concat(),
            b"NEL NEL NEL "
        );
    }

    #[test]
    fn frequency_leaf_reports_tsc_and_apic_khz() {
        assert_eq!(
            hypervisor_frequency_leaf(2_300_000),
            (2_300_000, 2_300_000, 0, 0)
        );
    }
}
