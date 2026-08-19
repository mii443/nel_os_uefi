use x86::vmx::vmcs;
use x86_64::{
    PhysAddr,
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
};

use crate::info;
use crate::memory::bitmap::BitmapMemoryTable;
use crate::vmm::x86_64::common::read_msr;
use crate::vmm::x86_64::intel::vcpu::IntelVCpu;
use crate::vmm::x86_64::intel::{vmread, vmwrite};

type MsrIndex = u32;

const EFER_SCE: u64 = 1 << 0;
const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;
const EFER_NXE: u64 = 1 << 11;
const EFER_SUPPORTED: u64 = EFER_SCE | EFER_LME | EFER_LMA | EFER_NXE;

const MAX_NUM_ENTS: usize = 4096 / core::mem::size_of::<SavedMsr>();

#[derive(Debug, Clone, Copy, Default)]
// Intel requires VM-entry/VM-exit MSR areas to be 16-byte aligned and each
// entry to occupy exactly 16 bytes.
#[repr(C, align(16))]
pub struct SavedMsr {
    pub index: MsrIndex,
    pub reserved: u32,
    pub data: u64,
}

#[derive(Debug)]
pub struct ShadowMsr {
    frame: PhysFrame,
    len: usize,
}

#[derive(Debug)]
pub enum MsrError {
    TooManyEntries,
    AreaAllocationFailed,
}

pub fn register_msrs(vcpu: &mut IntelVCpu) -> Result<(), MsrError> {
    vcpu.host_msr
        .set(x86::msr::IA32_TSC_AUX, read_msr(x86::msr::IA32_TSC_AUX))?;
    vcpu.host_msr
        .set(x86::msr::IA32_STAR, read_msr(x86::msr::IA32_STAR))
        .unwrap();
    vcpu.host_msr
        .set(x86::msr::IA32_LSTAR, read_msr(x86::msr::IA32_LSTAR))
        .unwrap();
    vcpu.host_msr
        .set(x86::msr::IA32_CSTAR, read_msr(x86::msr::IA32_CSTAR))
        .unwrap();
    vcpu.host_msr
        .set(x86::msr::IA32_FMASK, read_msr(x86::msr::IA32_FMASK))
        .unwrap();
    vcpu.host_msr
        .set(
            x86::msr::IA32_KERNEL_GSBASE,
            read_msr(x86::msr::IA32_KERNEL_GSBASE),
        )
        .unwrap();

    vcpu.guest_msr.set(x86::msr::IA32_TSC_AUX, 0).unwrap();
    vcpu.guest_msr.set(x86::msr::IA32_STAR, 0).unwrap();
    vcpu.guest_msr.set(x86::msr::IA32_LSTAR, 0).unwrap();
    vcpu.guest_msr.set(x86::msr::IA32_CSTAR, 0).unwrap();
    vcpu.guest_msr.set(x86::msr::IA32_FMASK, 0).unwrap();
    vcpu.guest_msr.set(x86::msr::IA32_KERNEL_GSBASE, 0).unwrap();
    /*vcpu.guest_msr.set(0x1b, 0).unwrap();
    vcpu.guest_msr.set(0xc0010007, 0).unwrap();
    vcpu.guest_msr.set(0xc0010117, 0).unwrap();*/

    vmwrite(
        vmcs::control::VMEXIT_MSR_LOAD_ADDR_FULL,
        vcpu.host_msr.phys().as_u64(),
    )
    .unwrap();
    vmwrite(
        vmcs::control::VMEXIT_MSR_STORE_ADDR_FULL,
        vcpu.guest_msr.phys().as_u64(),
    )
    .unwrap();
    vmwrite(
        vmcs::control::VMENTRY_MSR_LOAD_ADDR_FULL,
        vcpu.guest_msr.phys().as_u64(),
    )
    .unwrap();

    Ok(())
}

pub fn _update_msrs(vcpu: &mut IntelVCpu) -> Result<(), MsrError> {
    info!("updating MSRs");
    for entry_index in 0..vcpu.host_msr.saved_ents().len() {
        let index = vcpu.host_msr.saved_ents()[entry_index].index;
        info!("{}", index);
        let value = read_msr(index);
        info!("Setting MSR {:#x} to {:#x}", index, value);
        vcpu.host_msr.set_by_index(index, value).unwrap();
    }

    vmwrite(
        vmcs::control::VMEXIT_MSR_LOAD_COUNT,
        vcpu.host_msr.saved_ents().len() as u64,
    )
    .unwrap();
    vmwrite(
        vmcs::control::VMEXIT_MSR_STORE_COUNT,
        vcpu.guest_msr.saved_ents().len() as u64,
    )
    .unwrap();
    vmwrite(
        vmcs::control::VMENTRY_MSR_LOAD_COUNT,
        vcpu.guest_msr.saved_ents().len() as u64,
    )
    .unwrap();
    Ok(())
}

impl ShadowMsr {
    pub fn new(frame_allocator: &mut dyn FrameAllocator<Size4KiB>) -> Result<Self, MsrError> {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MsrError::AreaAllocationFailed)?;
        unsafe {
            core::ptr::write_bytes(frame.start_address().as_u64() as *mut u8, 0, 4096);
        }
        Ok(Self { frame, len: 0 })
    }

    fn entries_ptr(&self) -> *mut SavedMsr {
        self.frame.start_address().as_u64() as *mut SavedMsr
    }

    fn saved_ents_mut(&mut self) -> &mut [SavedMsr] {
        unsafe { core::slice::from_raw_parts_mut(self.entries_ptr(), self.len) }
    }

    pub fn clear(&mut self) {
        self.len = 0;
        unsafe {
            core::ptr::write_bytes(self.entries_ptr() as *mut u8, 0, 4096);
        }
    }

    pub fn set(&mut self, index: MsrIndex, data: u64) -> Result<(), MsrError> {
        self.set_by_index(index, data)
    }

    pub fn set_by_index(&mut self, index: MsrIndex, data: u64) -> Result<(), MsrError> {
        if let Some(entry) = self.saved_ents_mut().iter_mut().find(|e| e.index == index) {
            entry.data = data;
            return Ok(());
        }

        if self.len >= MAX_NUM_ENTS {
            return Err(MsrError::TooManyEntries);
        }
        unsafe {
            self.entries_ptr().add(self.len).write(SavedMsr {
                index,
                reserved: 0,
                data,
            });
        }
        self.len += 1;
        Ok(())
    }

    pub fn saved_ents(&self) -> &[SavedMsr] {
        unsafe { core::slice::from_raw_parts(self.entries_ptr(), self.len) }
    }

    pub fn find(&self, index: MsrIndex) -> Option<&SavedMsr> {
        self.saved_ents().iter().find(|e| e.index == index)
    }

    pub fn phys(&self) -> PhysAddr {
        self.frame.start_address()
    }

    pub fn reclaim(self, allocator: &mut BitmapMemoryTable) {
        allocator.deallocate_frame(self.frame);
    }

    pub fn concat(r1: u64, r2: u64) -> u64 {
        ((r1 & 0xFFFFFFFF) << 32) | (r2 & 0xFFFFFFFF)
    }

    pub fn set_ret_val(vcpu: &mut IntelVCpu, val: u64) {
        vcpu.guest_registers.rdx = (val >> 32) as u32 as u64;
        vcpu.guest_registers.rax = val as u32 as u64;
    }

    pub fn shadow_read(vcpu: &mut IntelVCpu, msr_kind: MsrIndex) -> Result<(), &'static str> {
        if let Some(msr) = vcpu.guest_msr.find(msr_kind) {
            Self::set_ret_val(vcpu, msr.data);
            Ok(())
        } else {
            Err("MSR is not present in the guest shadow")
        }
    }

    pub fn shadow_write(vcpu: &mut IntelVCpu, msr_kind: MsrIndex) -> Result<(), &'static str> {
        let regs = &vcpu.guest_registers;
        let value = Self::concat(regs.rdx, regs.rax);
        if let Some(msr) = vcpu
            .guest_msr
            .saved_ents_mut()
            .iter_mut()
            .find(|entry| entry.index == msr_kind)
        {
            msr.data = value;
            Ok(())
        } else {
            Err("MSR is not present in the guest shadow")
        }
    }

    pub fn handle_read_msr_vmexit(vcpu: &mut IntelVCpu) -> Result<(), &'static str> {
        let msr_kind = vcpu.guest_registers.rcx as u32;

        match msr_kind {
            x86::msr::IA32_EFER => Self::set_ret_val(vcpu, vmread(vmcs::guest::IA32_EFER_FULL)?),
            x86::msr::IA32_TIME_STAMP_COUNTER => {
                Self::set_ret_val(vcpu, unsafe { x86::time::rdtsc() })
            }
            x86::msr::IA32_FEATURE_CONTROL => {
                // Lock bit (0) | Enable VMX inside SMX (1) | Enable VMX outside SMX (2)
                Self::set_ret_val(vcpu, 0x5)
            }
            0x17 => Self::set_ret_val(vcpu, read_msr(0x17)), // IA32_PLATFORM_ID
            0x48 => Self::set_ret_val(vcpu, 0),              // IA32_SPEC_CTRL
            0x122 => Self::set_ret_val(vcpu, 0),             // IA32_TSX_CTRL
            0x560 => Self::set_ret_val(vcpu, 0),             // IA32_RTIT_OUTPUT_BASE
            0x561 => Self::set_ret_val(vcpu, 0),             // IA32_RTIT_OUTPUT_MASK_PTRS
            0x570 => Self::set_ret_val(vcpu, 0),             // IA32_RTIT_CTL
            0x571 => Self::set_ret_val(vcpu, 0),             // IA32_RTIT_STATUS
            0x572 => Self::set_ret_val(vcpu, 0),             // IA32_CR3_MATCH
            0x580 => Self::set_ret_val(vcpu, 0),             // IA32_ADDR0_START
            0x581 => Self::set_ret_val(vcpu, 0),             // IA32_ADDR0_END
            0x582 => Self::set_ret_val(vcpu, 0),             // IA32_ADDR1_START
            0x583 => Self::set_ret_val(vcpu, 0),             // IA32_ADDR1_END
            0x584 => Self::set_ret_val(vcpu, 0),             // IA32_ADDR2_START
            0x585 => Self::set_ret_val(vcpu, 0),             // IA32_ADDR2_END
            0x586 => Self::set_ret_val(vcpu, 0),             // IA32_ADDR3_START
            0x587 => Self::set_ret_val(vcpu, 0),             // IA32_ADDR3_END
            x86::msr::IA32_FS_BASE => Self::set_ret_val(vcpu, vmread(vmcs::guest::FS_BASE)?),
            x86::msr::IA32_GS_BASE => Self::set_ret_val(vcpu, vmread(vmcs::guest::GS_BASE)?),
            x86::msr::IA32_KERNEL_GSBASE => Self::shadow_read(vcpu, msr_kind)?,
            x86::msr::IA32_STAR => Self::shadow_read(vcpu, msr_kind)?,
            x86::msr::IA32_LSTAR => Self::shadow_read(vcpu, msr_kind)?,
            x86::msr::IA32_CSTAR => Self::shadow_read(vcpu, msr_kind)?,
            x86::msr::IA32_FMASK => Self::shadow_read(vcpu, msr_kind)?,
            x86::msr::SYSENTER_CS_MSR => {
                Self::set_ret_val(vcpu, vmread(vmcs::guest::IA32_SYSENTER_CS)?)
            }
            x86::msr::SYSENTER_ESP_MSR => {
                Self::set_ret_val(vcpu, vmread(vmcs::guest::IA32_SYSENTER_ESP)?)
            }
            x86::msr::SYSENTER_EIP_MSR => {
                Self::set_ret_val(vcpu, vmread(vmcs::guest::IA32_SYSENTER_EIP)?)
            }
            0x1b => Self::set_ret_val(vcpu, vcpu.guest_apic_base),
            0x1a0 => Self::set_ret_val(vcpu, vcpu.guest_misc_enable), // IA32_MISC_ENABLE
            0x802..=0x83f => {
                let value = vcpu
                    .local_apic
                    .read_x2apic(msr_kind)
                    .ok_or("Unsupported guest x2APIC RDMSR")?;
                Self::set_ret_val(vcpu, value);
            }
            0x8b => Self::set_ret_val(vcpu, 0x8701021),
            0xc0011029 => Self::set_ret_val(vcpu, 0x3000310e08202),
            0xc0010000 => Self::set_ret_val(vcpu, 0x130076),
            0xc0010001 => Self::set_ret_val(vcpu, 0),
            0xc0010002 => Self::set_ret_val(vcpu, 0),
            0xc0010003 => Self::set_ret_val(vcpu, 0),
            0xc0010007 => Self::set_ret_val(vcpu, 0),
            0xc0010114 => Self::set_ret_val(vcpu, 0),
            0xc0010117 => Self::set_ret_val(vcpu, 0), // MSR_VM_HSAVE_PA
            0x277 => Self::set_ret_val(vcpu, vmread(vmcs::guest::IA32_PAT_FULL)?),
            0xc0000103 => Self::shadow_read(vcpu, msr_kind)?, // TSC_AUX
            0xd90 => Self::set_ret_val(vcpu, 0),              // MSR_C1_PMON_EVNT_SEL0
            0xe1 => Self::set_ret_val(vcpu, 0),               // IA32_UMWAIT_CONTROL
            0x1c4 => Self::set_ret_val(vcpu, 0),              // Unknown MSR
            0x1c5 => Self::set_ret_val(vcpu, 0),              // Unknown MSR
            _ => return Err("Unsupported guest RDMSR"),
        }

        Ok(())
    }

    pub fn handle_wrmsr_vmexit(vcpu: &mut IntelVCpu) -> Result<(), &'static str> {
        let regs = &vcpu.guest_registers;
        let value = Self::concat(regs.rdx, regs.rax);
        let msr_kind: MsrIndex = regs.rcx as MsrIndex;

        match msr_kind {
            x86::msr::IA32_STAR => Self::shadow_write(vcpu, msr_kind)?,
            x86::msr::IA32_LSTAR => Self::shadow_write(vcpu, msr_kind)?,
            x86::msr::IA32_CSTAR => Self::shadow_write(vcpu, msr_kind)?,
            x86::msr::IA32_TSC_AUX => Self::shadow_write(vcpu, msr_kind)?,
            x86::msr::IA32_FMASK => Self::shadow_write(vcpu, msr_kind)?,
            x86::msr::IA32_KERNEL_GSBASE => Self::shadow_write(vcpu, msr_kind)?,
            x86::msr::MSR_C5_PMON_BOX_CTRL => Self::shadow_write(vcpu, msr_kind)?,
            x86::msr::SYSENTER_CS_MSR => vmwrite(vmcs::guest::IA32_SYSENTER_CS, value)?,
            x86::msr::SYSENTER_EIP_MSR => vmwrite(vmcs::guest::IA32_SYSENTER_EIP, value)?,
            x86::msr::SYSENTER_ESP_MSR => vmwrite(vmcs::guest::IA32_SYSENTER_ESP, value)?,
            x86::msr::IA32_EFER => {
                info!("Setting IA32_EFER: {:#x}", value);
                let current = vmread(vmcs::guest::IA32_EFER_FULL)?;
                let cr0 = vmread(vmcs::guest::CR0)?;
                let value = validate_efer_write(value, current, cr0)?;
                vmwrite(vmcs::guest::IA32_EFER_FULL, value)?;
                super::cr::update_ia32e(vcpu)?;
            }
            x86::msr::IA32_FS_BASE => vmwrite(vmcs::guest::FS_BASE, value)?,
            x86::msr::IA32_GS_BASE => vmwrite(vmcs::guest::GS_BASE, value)?,
            0x1b => vcpu.guest_apic_base = value & 0xffff_f000 | (value & 0xd00),
            0x1a0 => {
                // IA32_MISC_ENABLE.XD-disable is the only writable bit needed
                // by generic guests.  Bits 11 and 12 describe unavailable
                // branch-trace/PEBS facilities and remain mandatory.
                const XD_DISABLE: u64 = 1 << 34;
                vcpu.guest_misc_enable =
                    (vcpu.guest_misc_enable & !XD_DISABLE) | (value & XD_DISABLE) | 0x1800;
            }
            0x277 => vmwrite(
                vmcs::guest::IA32_PAT_FULL,
                crate::vmm::x86_64::common::msr::validate_pat(value)?,
            )?,
            0x802..=0x83f => {
                vcpu.local_apic
                    .write_x2apic(msr_kind, value)
                    .ok_or("Unsupported guest x2APIC WRMSR")?;
                if msr_kind == 0x80b && vcpu.local_apic.eoi() {
                    vcpu.io_apic.eoi();
                }
            }
            0xc0010007 => Self::shadow_write(vcpu, msr_kind)?,
            0xc0010117 => Self::shadow_write(vcpu, msr_kind)?,

            _ => return Err("Unsupported guest WRMSR"),
        }

        Ok(())
    }
}

fn validate_efer_write(value: u64, current: u64, cr0: u64) -> Result<u64, &'static str> {
    if value & !EFER_SUPPORTED != 0 {
        return Err("Invalid guest IA32_EFER value");
    }
    if cr0 & (1 << 31) != 0 && (value ^ current) & EFER_LME != 0 {
        return Err("Guest IA32_EFER.LME cannot change while paging is enabled");
    }

    // LMA is read-only. Preserve it here; update_ia32e derives it again from
    // LME, CR0.PG, and CR4.PAE after the write.
    Ok((value & !EFER_LMA) | (current & EFER_LMA))
}

#[cfg(test)]
mod tests {
    use super::{EFER_LMA, validate_efer_write};

    #[test]
    fn efer_write_accepts_ovmf_and_linux_values() {
        assert_eq!(validate_efer_write(0x100, 0, 0).unwrap(), 0x100);
        assert_eq!(validate_efer_write(0xd00, 0x500, 1 << 31).unwrap(), 0xd00);
        assert_eq!(validate_efer_write(0xd01, 0x500, 1 << 31).unwrap(), 0xd01);
    }

    #[test]
    fn efer_write_preserves_read_only_lma_and_rejects_invalid_changes() {
        assert_eq!(validate_efer_write(0x100, EFER_LMA, 0).unwrap(), 0x500);
        assert!(validate_efer_write(0, 0x100, 1 << 31).is_err());
        assert!(validate_efer_write(1 << 63, 0, 0).is_err());
    }
}
