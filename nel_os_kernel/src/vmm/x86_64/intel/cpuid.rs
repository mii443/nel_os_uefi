use modular_bitfield::bitfield;
use raw_cpuid::cpuid;

use crate::vmm::x86_64::intel::vcpu::IntelVCpu;

pub fn handle_cpuid_vmexit(vcpu: &mut IntelVCpu) {
    let regs = &mut vcpu.guest_registers;

    let vendor: &[u8; 12] = b"miHypervisor";
    let brand_string: &[u8; 48] = b"mii Hypervisor CPU on Intel VT-x               \0";
    let vendor = unsafe { core::mem::transmute::<&[u8; 12], &[u32; 3]>(vendor) };
    let brand_string = unsafe { core::mem::transmute::<&[u8; 48], &[u32; 12]>(brand_string) };

    match VmxLeaf::from(regs.rax) {
        VmxLeaf::EXTENDED_FEATURE_2 => {
            regs.rax = brand_string[0] as u64;
            regs.rbx = brand_string[1] as u64;
            regs.rcx = brand_string[2] as u64;
            regs.rdx = brand_string[3] as u64;
        }
        VmxLeaf::EXTENDED_FEATURE_3 => {
            regs.rax = brand_string[4] as u64;
            regs.rbx = brand_string[5] as u64;
            regs.rcx = brand_string[6] as u64;
            regs.rdx = brand_string[7] as u64;
        }
        VmxLeaf::EXTENDED_FEATURE_4 => {
            regs.rax = brand_string[8] as u64;
            regs.rbx = brand_string[9] as u64;
            regs.rcx = brand_string[10] as u64;
            regs.rdx = brand_string[11] as u64;
        }
        VmxLeaf::EXTENDED_ENUMERATION => match regs.rcx {
            0 => {
                regs.rax = 0b11;
                regs.rbx = 576;
                regs.rcx = 576;
                regs.rdx = 0x00000000;
            }
            1 => {
                regs.rax = 0x00000001;
                regs.rbx = 0;
                regs.rcx = 0;
                regs.rdx = 0;
            }
            2 => {
                regs.rax = 512;
                regs.rbx = 0;
                regs.rcx = 0;
                regs.rdx = 0;
            }
            _ => {
                invalid(vcpu);
            }
        },
        VmxLeaf::EXTENDED_FEATURE => match regs.rcx {
            0 => {
                let ebx = ExtFeatureEbx0::new()
                    .with_fsgsbase(false)
                    .with_smep(true)
                    .with_invpcid(false)
                    .with_smap(true);
                regs.rax = 1;
                regs.rbx = u32::from(ebx) as u64;
                regs.rcx = 0;
                regs.rdx = 0;
            }
            1 => {
                invalid(vcpu);
            }
            2 => {
                invalid(vcpu);
            }
            _ => {
                panic!("Unhandled CPUID leaf: {:#x}.{:#x}", regs.rax, regs.rcx);
            }
        },
        VmxLeaf::EXTENDED_PROCESSOR_SIGNATURE => {
            let signature = cpuid!(0x80000001, 0);
            regs.rax = 0x00000000;
            regs.rbx = 0x00000000;
            regs.rcx = signature.ecx as u64;
            regs.rdx = signature.edx as u64;
        }
        VmxLeaf::EXTENDED_FUNCTION => {
            regs.rax = 0x80000000 + 4;
            regs.rbx = 0x00000000;
            regs.rcx = 0x00000000;
            regs.rdx = 0x00000000;
        }
        VmxLeaf::MAXIMUM_INPUT => {
            regs.rax = 0x20;
            regs.rbx = vendor[0] as u64;
            regs.rcx = vendor[2] as u64;
            regs.rdx = vendor[1] as u64;
        }
        VmxLeaf::VERSION_AND_FEATURE_INFO => {
            let ecx = FeatureInfoEcx::new()
                .with_pcid(true)
                .with_sse4_1(true)
                .with_sse4_2(true)
                .with_xsave(true)
                .with_osxsave(true);

            let edx = FeatureInfoEdx::new()
                .with_fpu(true)
                .with_vme(true)
                .with_de(true)
                .with_pse(true)
                .with_msr(true)
                .with_pae(true)
                .with_cx8(true)
                .with_sep(true)
                .with_pge(true)
                .with_cmov(true)
                .with_pse36(true)
                .with_acpi(true)
                .with_fxsr(true)
                .with_sse(true)
                .with_sse2(true);

            let mut version_and_feature_info = cpuid!(0x1, 0);
            version_and_feature_info.ecx &= !(1 << 17);

            regs.rax = version_and_feature_info.eax as u64;
            regs.rbx = version_and_feature_info.ebx as u64;
            regs.rcx = u32::from(ecx) as u64;
            regs.rdx = u32::from(edx) as u64;
        }
        _ => {
            invalid(vcpu);
        }
    }
}

fn invalid(vcpu: &mut IntelVCpu) {
    let regs = &mut vcpu.guest_registers;

    regs.rax = 0;
    regs.rbx = 0;
    regs.rcx = 0;
    regs.rdx = 0;
}

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct FeatureInfoEcx {
    pub sse3: bool,
    pub pclmulqdq: bool,
    pub dtes64: bool,
    pub monitor: bool,
    pub ds_cpl: bool,
    pub vmx: bool,
    pub smx: bool,
    pub eist: bool,
    pub tm2: bool,
    pub ssse3: bool,
    pub cnxt_id: bool,
    pub sdbg: bool,
    pub fma: bool,
    pub cmpxchg16b: bool,
    pub xtpr: bool,
    pub pdcm: bool,
    pub _reserved_0: bool,
    pub pcid: bool,
    pub dca: bool,
    pub sse4_1: bool,
    pub sse4_2: bool,
    pub x2apic: bool,
    pub movbe: bool,
    pub popcnt: bool,
    pub tsc_deadline: bool,
    pub aesni: bool,
    pub xsave: bool,
    pub osxsave: bool,
    pub avx: bool,
    pub f16c: bool,
    pub rdrand: bool,
    pub hypervisor: bool,
}

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct FeatureInfoEdx {
    pub fpu: bool,
    pub vme: bool,
    pub de: bool,
    pub pse: bool,
    pub tsc: bool,
    pub msr: bool,
    pub pae: bool,
    pub mce: bool,
    pub cx8: bool,
    pub apic: bool,
    pub _reserved_0: bool,
    pub sep: bool,
    pub mtrr: bool,
    pub pge: bool,
    pub mca: bool,
    pub cmov: bool,
    pub pat: bool,
    pub pse36: bool,
    pub psn: bool,
    pub clfsh: bool,
    pub _reserved_1: bool,
    pub ds: bool,
    pub acpi: bool,
    pub mmx: bool,
    pub fxsr: bool,
    pub sse: bool,
    pub sse2: bool,
    pub ss: bool,
    pub htt: bool,
    pub tm: bool,
    pub _reserved_2: bool,
    pub pbe: bool,
}

#[bitfield]
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub struct ExtFeatureEbx0 {
    pub fsgsbase: bool,
    pub tsc_adjust: bool,
    pub sgx: bool,
    pub bmi1: bool,
    pub hle: bool,
    pub avx2: bool,
    pub fdp: bool,
    pub smep: bool,
    pub bmi2: bool,
    pub erms: bool,
    pub invpcid: bool,
    pub rtm: bool,
    pub rdtm: bool,
    pub fpucsds: bool,
    pub mpx: bool,
    pub rdta: bool,
    pub avx512f: bool,
    pub avx512dq: bool,
    pub rdseed: bool,
    pub adx: bool,
    pub smap: bool,
    pub avx512ifma: bool,
    pub _reserved1: bool,
    pub clflushopt: bool,
    pub clwb: bool,
    pub pt: bool,
    pub avx512pf: bool,
    pub avx512er: bool,
    pub avx512cd: bool,
    pub sha: bool,
    pub avx512bw: bool,
    pub avx512vl: bool,
}

pub enum VmxLeaf {
    MAXIMUM_INPUT = 0x0,
    VERSION_AND_FEATURE_INFO = 0x1,
    EXTENDED_FEATURE = 0x7,
    EXTENDED_ENUMERATION = 0xD,
    EXTENDED_FUNCTION = 0x80000000,
    EXTENDED_PROCESSOR_SIGNATURE = 0x80000001,
    EXTENDED_FEATURE_2 = 0x80000002,
    EXTENDED_FEATURE_3 = 0x80000003,
    EXTENDED_FEATURE_4 = 0x80000004,
    UNKNOWN = 0xFFFFFFFF,
}

impl VmxLeaf {
    pub fn from(rax: u64) -> VmxLeaf {
        match rax {
            0x0 => VmxLeaf::MAXIMUM_INPUT,
            0x1 => VmxLeaf::VERSION_AND_FEATURE_INFO,
            0x7 => VmxLeaf::EXTENDED_FEATURE,
            0xD => VmxLeaf::EXTENDED_ENUMERATION,
            0x80000000 => VmxLeaf::EXTENDED_FUNCTION,
            0x80000001 => VmxLeaf::EXTENDED_PROCESSOR_SIGNATURE,
            0x80000002 => VmxLeaf::EXTENDED_FEATURE_2,
            0x80000003 => VmxLeaf::EXTENDED_FEATURE_3,
            0x80000004 => VmxLeaf::EXTENDED_FEATURE_4,
            _ => VmxLeaf::UNKNOWN,
        }
    }
}
