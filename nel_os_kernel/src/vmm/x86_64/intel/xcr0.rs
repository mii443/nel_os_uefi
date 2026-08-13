const XCR0_X87: u64 = 1 << 0;
const XCR0_SSE: u64 = 1 << 1;
const XCR0_AVX: u64 = 1 << 2;
const XCR0_MPX: u64 = (1 << 3) | (1 << 4);
const XCR0_AVX512: u64 = (1 << 5) | (1 << 6) | (1 << 7);
const XCR0_AMX: u64 = (1 << 17) | (1 << 18);

pub const GUEST_XCR0_MASK: u64 = XCR0_X87 | XCR0_SSE;

pub fn validate(index: u32, xcr0: u64, supported: u64) -> Result<(), &'static str> {
    if index != 0 {
        return Err("Invalid XCR index");
    }

    if xcr0 & !supported != 0 {
        return Err("XCR0 contains a state component unsupported by the host");
    }

    if xcr0 & XCR0_X87 == 0 {
        return Err("X87 is not enabled");
    }

    if xcr0 & XCR0_AVX != 0 && xcr0 & XCR0_SSE == 0 {
        return Err("SSE is not enabled");
    }

    if xcr0 & XCR0_MPX != 0 && xcr0 & XCR0_MPX != XCR0_MPX {
        return Err("BNDREGS and BNDCSR are not both enabled");
    }

    if xcr0 & XCR0_AVX512 != 0 {
        if xcr0 & XCR0_AVX == 0 {
            return Err("YMM bits are not enabled");
        }

        if xcr0 & XCR0_AVX512 != XCR0_AVX512 {
            return Err("Invalid bits set in XCR0");
        }
    }

    if xcr0 & XCR0_AMX != 0 && xcr0 & XCR0_AMX != XCR0_AMX {
        return Err("XTILECFG and XTILEDATA are not both enabled");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_TEST_FEATURES: u64 =
        XCR0_X87 | XCR0_SSE | XCR0_AVX | XCR0_MPX | XCR0_AVX512 | XCR0_AMX;

    #[test]
    fn accepts_supported_dependency_complete_xcr0() {
        assert!(validate(0, ALL_TEST_FEATURES, ALL_TEST_FEATURES).is_ok());
    }

    #[test]
    fn rejects_unsupported_component() {
        assert!(validate(0, XCR0_X87 | XCR0_SSE, XCR0_X87).is_err());
    }

    #[test]
    fn rejects_missing_x87() {
        assert!(validate(0, XCR0_SSE, ALL_TEST_FEATURES).is_err());
    }

    #[test]
    fn rejects_avx_without_sse() {
        assert!(validate(0, XCR0_X87 | XCR0_AVX, ALL_TEST_FEATURES).is_err());
    }

    #[test]
    fn rejects_partial_component_groups() {
        assert!(validate(0, XCR0_X87 | (1 << 3), ALL_TEST_FEATURES).is_err());
        assert!(
            validate(
                0,
                XCR0_X87 | XCR0_SSE | XCR0_AVX | (1 << 5),
                ALL_TEST_FEATURES,
            )
            .is_err()
        );
        assert!(validate(0, XCR0_X87 | (1 << 17), ALL_TEST_FEATURES).is_err());
    }

    #[test]
    fn rejects_nonzero_xcr_index() {
        assert!(validate(1, XCR0_X87, ALL_TEST_FEATURES).is_err());
    }
}
