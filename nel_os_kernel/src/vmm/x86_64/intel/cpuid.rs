#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
use modular_bitfield::bitfield;
use raw_cpuid::cpuid;

use crate::vmm::x86_64::common::cpuid::{
    HYPERVISOR_BASE_LEAF, HYPERVISOR_FREQUENCY_LEAF, KVM_COMPAT_BASE_LEAF,
    hypervisor_frequency_leaf, hypervisor_vendor_leaf, kvm_compat_vendor_leaf,
};
use crate::vmm::x86_64::intel::vcpu::IntelVCpu;

pub fn handle_cpuid_vmexit(vcpu: &mut IntelVCpu) {
    let regs = &mut vcpu.guest_registers;

    let brand_string: &[u8; 48] = b"mii Hypervisor CPU on Intel VT-x               \0";
    let brand_string = unsafe { core::mem::transmute::<&[u8; 48], &[u32; 12]>(brand_string) };

    if regs.rax as u32 == HYPERVISOR_BASE_LEAF {
        let (eax, ebx, ecx, edx) = hypervisor_vendor_leaf();
        regs.rax = u64::from(eax);
        regs.rbx = u64::from(ebx);
        regs.rcx = u64::from(ecx);
        regs.rdx = u64::from(edx);
        return;
    }

    if regs.rax as u32 == HYPERVISOR_FREQUENCY_LEAF {
        let tsc_khz = crate::interrupt::apic::GUEST_TSC_KHZ
            .get()
            .copied()
            .unwrap_or(0)
            .min(u32::MAX as u64) as u32;
        let (eax, ebx, ecx, edx) = hypervisor_frequency_leaf(tsc_khz);
        regs.rax = u64::from(eax);
        regs.rbx = u64::from(ebx);
        regs.rcx = u64::from(ecx);
        regs.rdx = u64::from(edx);
        return;
    }

    if regs.rax as u32 == KVM_COMPAT_BASE_LEAF {
        let (eax, ebx, ecx, edx) = kvm_compat_vendor_leaf();
        regs.rax = u64::from(eax);
        regs.rbx = u64::from(ebx);
        regs.rcx = u64::from(ecx);
        regs.rdx = u64::from(edx);
        return;
    }

    if regs.rax as u32 == KVM_COMPAT_BASE_LEAF + 1 {
        invalid(vcpu);
        return;
    }

    if (HYPERVISOR_BASE_LEAF + 1..KVM_COMPAT_BASE_LEAF).contains(&(regs.rax as u32)) {
        invalid(vcpu);
        return;
    }

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
                let ebx = guest_leaf7_ebx(cpuid!(0x7, 0).ebx);
                regs.rax = 1;
                regs.rbx = ebx as u64;
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
                // Architecturally unsupported structured-feature subleaves
                // return zero.  The subleaf is guest-controlled, so it must
                // never be allowed to panic the host.
                invalid(vcpu);
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
            let vendor = cpuid!(0, 0);
            regs.rax = u64::from(vendor.eax);
            regs.rbx = u64::from(vendor.ebx);
            regs.rcx = u64::from(vendor.ecx);
            regs.rdx = u64::from(vendor.edx);
        }
        VmxLeaf::VERSION_AND_FEATURE_INFO => {
            let version_and_feature_info = cpuid!(0x1, 0);
            let ecx = guest_leaf1_ecx(version_and_feature_info.ecx);

            let edx = guest_leaf1_edx();

            regs.rax = version_and_feature_info.eax as u64;
            regs.rbx = single_vcpu_leaf1_ebx(version_and_feature_info.ebx) as u64;
            regs.rcx = ecx as u64;
            regs.rdx = u64::from(edx);
        }
        _ => {
            invalid(vcpu);
        }
    }
}

fn guest_leaf1_edx() -> u32 {
    FeatureInfoEdx::new()
        .with_fpu(true)
        .with_vme(true)
        .with_de(true)
        .with_pse(true)
        .with_msr(true)
        .with_pae(true)
        .with_cx8(true)
        .with_apic(true)
        .with_sep(true)
        .with_pge(true)
        .with_cmov(true)
        .with_pat(true)
        .with_pse36(true)
        .with_acpi(true)
        .with_fxsr(true)
        .with_sse(true)
        .with_sse2(true)
        .into()
}

fn guest_leaf1_ecx(host_ecx: u32) -> u32 {
    FeatureInfoEcx::new()
        .with_pcid(true)
        .with_sse4_1(true)
        .with_sse4_2(true)
        .with_x2apic(true)
        .with_xsave(true)
        .with_osxsave(true)
        .with_rdrand(host_ecx & (1 << 30) != 0)
        .with_hypervisor(true)
        .into()
}

fn guest_leaf7_ebx(host_ebx: u32) -> u32 {
    // Instructions advertised here execute directly in VMX non-root mode.
    // Never advertise a feature that the physical CPU cannot execute: doing
    // so turns hot kernel paths such as STAC/CLAC into repeated #UD exits.
    ExtFeatureEbx0::new()
        .with_fsgsbase(false)
        .with_smep(host_ebx & (1 << 7) != 0)
        .with_invpcid(false)
        .with_rdseed(host_ebx & (1 << 18) != 0)
        .with_smap(host_ebx & (1 << 20) != 0)
        .into()
}

fn single_vcpu_leaf1_ebx(host_ebx: u32) -> u32 {
    // CPUID.1:EBX[23:16] is the maximum number of addressable logical
    // processors and EBX[31:24] is the initial APIC ID. Report topology that
    // matches this single-VCPU VM rather than passing through the host values.
    (host_ebx & 0x0000_ffff) | (1 << 16)
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

#[allow(clippy::enum_clike_unportable_variant)]
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
    Unknown = 0xFFFFFFFF,
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
            _ => VmxLeaf::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{guest_leaf1_ecx, guest_leaf1_edx, guest_leaf7_ebx, single_vcpu_leaf1_ebx};

    #[test]
    fn leaf1_ebx_describes_one_vcpu_with_apic_id_zero() {
        let ebx = single_vcpu_leaf1_ebx(0xab20_0800);

        assert_eq!((ebx >> 16) & 0xff, 1);
        assert_eq!(ebx >> 24, 0);
        assert_eq!(ebx & 0xffff, 0x0800);
    }

    #[test]
    fn native_random_features_follow_the_host() {
        assert_eq!(guest_leaf1_ecx(0) & (1 << 30), 0);
        assert_ne!(guest_leaf1_ecx(1 << 30) & (1 << 30), 0);
        assert_eq!(guest_leaf7_ebx(0) & (1 << 18), 0);
        assert_ne!(guest_leaf7_ebx(1 << 18) & (1 << 18), 0);
    }

    #[test]
    fn leaf1_identifies_a_hypervisor() {
        assert_ne!(guest_leaf1_ecx(0) & (1 << 31), 0);
    }

    #[test]
    fn leaf1_pat_matches_the_virtual_msr_contract() {
        assert_ne!(guest_leaf1_edx() & (1 << 16), 0);
    }

    #[test]
    fn smep_and_smap_are_never_advertised_without_host_support() {
        let ebx = guest_leaf7_ebx(1 << 7);

        assert_ne!(ebx & (1 << 7), 0);
        assert_eq!(ebx & (1 << 20), 0);
    }
}
