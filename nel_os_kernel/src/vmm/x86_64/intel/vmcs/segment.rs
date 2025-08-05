use modular_bitfield::{bitfield, prelude::*};

#[derive(Specifier, Debug, Clone, Copy)]
pub enum DescriptorType {
    System = 0,
    Code = 1,
}

#[derive(Specifier, Debug, Clone, Copy)]
pub enum Granularity {
    Byte = 0,
    KByte = 1,
}

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct SegmentRights {
    pub accessed: bool,
    pub rw: bool,
    pub dc: bool,
    pub executable: bool,
    #[bits = 1]
    pub desc_type: DescriptorType,
    pub dpl: B2,
    pub present: bool,
    reserved: B4,
    pub avl: bool,
    pub long: bool,
    pub db: bool,
    #[bits = 1]
    pub granularity: Granularity,
    pub usable: bool,
    reserved2: B15,
}

impl Default for SegmentRights {
    fn default() -> Self {
        SegmentRights::new()
            .with_accessed(true)
            .with_present(true)
            .with_avl(false)
            .with_long(false)
            .with_usable(false)
    }
}
