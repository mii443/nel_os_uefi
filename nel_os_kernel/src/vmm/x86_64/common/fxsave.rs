/// Legacy x87, MXCSR, and XMM0-15 state in the architectural FXSAVE format.
///
/// `fxsave64`/`fxrstor64` require a 16-byte aligned 512-byte region. Keeping
/// the representation opaque also prevents Rust code from accidentally
/// depending on reserved fields in the hardware-defined format.
#[repr(C, align(16))]
pub struct FxState {
    bytes: [u8; 512],
}

impl FxState {
    pub fn guest_default() -> Self {
        let mut bytes = [0; 512];

        // FCW = all exceptions masked, double precision, round-to-nearest.
        bytes[0] = 0x7f;
        bytes[1] = 0x03;

        // MXCSR = all exceptions masked, round-to-nearest. All other fields,
        // including FSW and the abridged FTW, are valid at zero.
        bytes[24] = 0x80;
        bytes[25] = 0x1f;

        Self { bytes }
    }

    pub const fn zeroed() -> Self {
        Self { bytes: [0; 512] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    #[test]
    fn has_architectural_layout() {
        assert_eq!(size_of::<FxState>(), 512);
        assert_eq!(align_of::<FxState>(), 16);
    }

    #[test]
    fn guest_default_has_valid_control_words() {
        let state = FxState::guest_default();
        assert_eq!(u16::from_le_bytes([state.bytes[0], state.bytes[1]]), 0x037f);
        assert_eq!(
            u32::from_le_bytes([
                state.bytes[24],
                state.bytes[25],
                state.bytes[26],
                state.bytes[27],
            ]),
            0x1f80
        );
    }
}
