//! CPUID leaves shared by the VMX and SVM backends.

/// The first hypervisor-specific leaf implemented by nel_os.
pub const HYPERVISOR_BASE_LEAF: u32 = 0x4000_0000;

/// Secondary namespace for the feature-less KVM compatibility contract.
///
/// Linux uses this well-known signature to determine that virtual x2APIC can
/// be used without physical interrupt remapping.  No KVM feature bits are
/// advertised, so the guest will not use KVM clocks, hypercalls, or MSRs.
pub const KVM_COMPAT_BASE_LEAF: u32 = 0x4000_0100;

/// Return the standard hypervisor vendor leaf.
///
/// CPUID places the twelve-byte vendor ID in EBX, ECX, EDX order.  Keep the
/// maximum leaf at the base until a versioned paravirtual interface is added;
/// unknown guests can still reliably identify the virtual-machine boundary.
pub const fn hypervisor_vendor_leaf() -> (u32, u32, u32, u32) {
    (
        HYPERVISOR_BASE_LEAF,
        u32::from_le_bytes(*b"NEL "),
        u32::from_le_bytes(*b"NEL "),
        u32::from_le_bytes(*b"NEL "),
    )
}

pub const fn kvm_compat_vendor_leaf() -> (u32, u32, u32, u32) {
    (
        KVM_COMPAT_BASE_LEAF + 1,
        u32::from_le_bytes(*b"KVMK"),
        u32::from_le_bytes(*b"VMKV"),
        u32::from_le_bytes(*b"M\0\0\0"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_leaf_has_a_twelve_byte_signature() {
        let (max_leaf, ebx, ecx, edx) = hypervisor_vendor_leaf();

        assert_eq!(max_leaf, HYPERVISOR_BASE_LEAF);
        assert_eq!(
            [ebx.to_le_bytes(), ecx.to_le_bytes(), edx.to_le_bytes()].concat(),
            b"NEL NEL NEL "
        );
    }

    #[test]
    fn kvm_compatibility_namespace_advertises_no_features() {
        let (max_leaf, ebx, ecx, edx) = kvm_compat_vendor_leaf();

        assert_eq!(max_leaf, KVM_COMPAT_BASE_LEAF + 1);
        assert_eq!(
            [ebx.to_le_bytes(), ecx.to_le_bytes(), edx.to_le_bytes()].concat(),
            b"KVMKVMKVM\0\0\0"
        );
    }
}
