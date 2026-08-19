//! Architecture-wide helpers for guest model-specific registers.

/// Architectural reset value used by firmware and operating systems.
pub const DEFAULT_PAT: u64 = 0x0007_0406_0007_0406;

/// Validate the eight IA32_PAT entries.
///
/// Each entry is an eight-bit memory type. Bits 7:3 are reserved and memory
/// types 2 and 3 are architecturally reserved, so WRMSR must raise #GP for
/// those encodings.
pub fn validate_pat(value: u64) -> Result<u64, &'static str> {
    for entry in value.to_le_bytes() {
        if entry & !0x07 != 0 || matches!(entry & 0x07, 2 | 3) {
            return Err("Invalid guest IA32_PAT value");
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_architectural_memory_types() {
        assert_eq!(validate_pat(DEFAULT_PAT), Ok(DEFAULT_PAT));
        assert_eq!(
            validate_pat(0x0706_0504_0100_0706),
            Ok(0x0706_0504_0100_0706)
        );
    }

    #[test]
    fn rejects_reserved_bits_and_types() {
        assert!(validate_pat(0x08).is_err());
        assert!(validate_pat(0x02).is_err());
        assert!(validate_pat(0x0300).is_err());
    }
}
