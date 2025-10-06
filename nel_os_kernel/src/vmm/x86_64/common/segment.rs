use x86::segmentation;

use crate::vmm::x86_64::{
    common::X86VCpu,
    intel::vmcs::segment::{
        DescriptorType as IntelDescriptorType, Granularity as IntelGranularity,
        SegmentRights as IntelSegmentRights,
    },
};

pub enum Segment {
    ES,
    CS,
    SS,
    DS,
    FS,
    GS,
    GDTR,
    LDTR,
    IDTR,
    TR,
}

#[derive(Debug, Clone, Copy)]
pub enum DescriptorType {
    System = 0,
    Code = 1,
}

#[derive(Debug, Clone, Copy)]
pub enum Granularity {
    Byte = 0,
    KByte = 1,
}

pub struct SegmentRights {
    pub accessed: bool,
    pub rw: bool,
    pub dc: bool,
    pub executable: bool,
    pub desc_type: DescriptorType,
    pub dpl: u8,
    pub present: bool,
    pub avl: bool,
    pub long: bool,
    pub db: bool,
    pub granularity: Granularity,
}

impl Default for SegmentRights {
    fn default() -> Self {
        SegmentRights {
            accessed: true,
            rw: false,
            dc: false,
            executable: false,
            desc_type: DescriptorType::Code,
            dpl: 0,
            present: true,
            avl: false,
            long: false,
            db: false,
            granularity: Granularity::Byte,
        }
    }
}

impl SegmentRights {
    pub fn to_amd_segment_attrib(&self) -> u16 {
        let mut value: u16 = 0;
        value |= self.accessed as u16;
        value |= (self.rw as u16) << 1;
        value |= (self.dc as u16) << 2;
        value |= (self.executable as u16) << 3;
        value |= (self.desc_type as u16) << 4;
        value |= (self.dpl as u16 & 0b11) << 5;
        value |= (self.present as u16) << 7;
        value |= (self.avl as u16) << 12;
        value |= (self.long as u16) << 13;
        value |= (self.db as u16) << 14;
        value |= (self.granularity as u16) << 15;
        value
    }

    pub fn to_intel_segment_rights(&self) -> IntelSegmentRights {
        let mut rights = IntelSegmentRights::new();
        rights.set_accessed(self.accessed);
        rights.set_rw(self.rw);
        rights.set_dc(self.dc);
        rights.set_executable(self.executable);
        rights.set_desc_type(match self.desc_type {
            DescriptorType::System => IntelDescriptorType::System,
            DescriptorType::Code => IntelDescriptorType::Code,
        });
        rights.set_dpl(self.dpl & 0b11);
        rights.set_present(self.present);
        rights.set_avl(self.avl);
        rights.set_long(self.long);
        rights.set_db(self.db);
        rights.set_granularity(match self.granularity {
            Granularity::Byte => IntelGranularity::Byte,
            Granularity::KByte => IntelGranularity::KByte,
        });
        rights
    }
}

pub fn setup_segments(vcpu: &mut impl X86VCpu) {
    let cs_right = SegmentRights {
        accessed: true,
        rw: true,
        dc: false,
        executable: true,
        desc_type: DescriptorType::Code,
        dpl: 0,
        present: true,
        avl: false,
        long: true,
        db: false,
        granularity: Granularity::KByte,
    };
    vcpu.set_segment_rights(Segment::CS, cs_right);

    let ds_right = SegmentRights {
        accessed: true,
        rw: true,
        dc: false,
        executable: false,
        desc_type: DescriptorType::Code,
        dpl: 0,
        present: true,
        avl: false,
        long: false,
        db: true,
        granularity: Granularity::KByte,
    };
    vcpu.set_segment_rights(Segment::DS, ds_right);

    let tr_right = SegmentRights {
        accessed: true,
        rw: true,
        dc: false,
        executable: true,
        desc_type: DescriptorType::System,
        dpl: 0,
        present: true,
        avl: false,
        long: false,
        db: false,
        granularity: Granularity::Byte,
    };
    vcpu.set_segment_rights(Segment::TR, tr_right);

    let ldtr_right = SegmentRights {
        accessed: false,
        rw: true,
        dc: false,
        executable: false,
        desc_type: DescriptorType::System,
        dpl: 0,
        present: true,
        avl: false,
        long: false,
        db: false,
        granularity: Granularity::Byte,
    };
    vcpu.set_segment_rights(Segment::LDTR, ldtr_right);

    vcpu.set_segment_base(Segment::CS, 0);
    vcpu.set_segment_base(Segment::DS, 0);
    vcpu.set_segment_base(Segment::ES, 0);
    vcpu.set_segment_base(Segment::FS, 0);
    vcpu.set_segment_base(Segment::GS, 0);
    vcpu.set_segment_base(Segment::SS, 0);
    vcpu.set_segment_base(Segment::TR, 0);
    vcpu.set_segment_base(Segment::GDTR, 0);
    vcpu.set_segment_base(Segment::IDTR, 0);
    vcpu.set_segment_base(Segment::LDTR, 0xDEAD00);

    vcpu.set_segment_limit(Segment::CS, u32::MAX);
    vcpu.set_segment_limit(Segment::DS, u32::MAX);
    vcpu.set_segment_limit(Segment::ES, u32::MAX);
    vcpu.set_segment_limit(Segment::FS, u32::MAX);
    vcpu.set_segment_limit(Segment::GS, u32::MAX);
    vcpu.set_segment_limit(Segment::SS, u32::MAX);
    vcpu.set_segment_limit(Segment::TR, 0);
    vcpu.set_segment_limit(Segment::GDTR, 0);
    vcpu.set_segment_limit(Segment::IDTR, 0);
    vcpu.set_segment_limit(Segment::LDTR, 0);

    vcpu.set_segment_selector(Segment::CS, segmentation::cs().bits() as u16);
    vcpu.set_segment_selector(Segment::DS, 0);
    vcpu.set_segment_selector(Segment::ES, 0);
    vcpu.set_segment_selector(Segment::FS, 0);
    vcpu.set_segment_selector(Segment::GS, 0);
    vcpu.set_segment_selector(Segment::SS, 0);
    vcpu.set_segment_selector(Segment::TR, 0);
    vcpu.set_segment_selector(Segment::LDTR, 0);
}
