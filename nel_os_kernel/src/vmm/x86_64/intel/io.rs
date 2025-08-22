use x86::vmx::vmcs;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

use super::qual::QualIo;
use crate::{
    info,
    vmm::x86_64::intel::{register::GuestRegisters, vmwrite},
};

#[derive(Default)]
pub struct Serial {
    pub ier: u8,
    pub mcr: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]

pub enum InitPhase {
    Uninitialized,
    Phase1,
    Phase2,
    Phase3,
    Initialized,
}

enum ReadSel {
    IRR,
    ISR,
}

pub struct PIC {
    pub primary_mask: u8,
    pub secondary_mask: u8,
    pub primary_phase: InitPhase,
    pub secondary_phase: InitPhase,
    pub primary_base: u8,
    pub secondary_base: u8,
    pub primary_irr: u8,
    pub primary_isr: u8,
    pub secondary_irr: u8,
    pub secondary_isr: u8,
    pub primary_read_sel: ReadSel,
    pub secondary_read_sel: ReadSel,
}

impl PIC {
    pub fn new() -> Self {
        Self {
            primary_mask: 0xFF,
            secondary_mask: 0xFF,
            primary_phase: InitPhase::Uninitialized,
            secondary_phase: InitPhase::Uninitialized,
            primary_base: 0,
            secondary_base: 0,
            primary_irr: 0,
            primary_isr: 0,
            secondary_irr: 0,
            secondary_isr: 0,
            primary_read_sel: ReadSel::IRR,
            secondary_read_sel: ReadSel::IRR,
        }
    }

    pub fn handle_io(&mut self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.direction() {
            0 => {
                self.handle_io_out(regs, qual);
            }
            1 => {
                self.handle_io_in(regs, qual);
            }
            _ => {}
        }
    }

    pub fn handle_io_in(&self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.port() {
            0x0CF8..=0x0CFF => regs.rax = 0,
            0xC000..=0xCFFF => {} //ignore
            0x20..=0x21 => self.handle_pic_in(regs, qual),
            0xA0..=0xA1 => self.handle_pic_in(regs, qual),
            0x0070..=0x0071 => regs.rax = 0,
            _ => regs.rax = 0,
        }
    }

    pub fn handle_io_out(&mut self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.port() {
            0x0CF8..=0x0CFF => {} //ignore
            0xC000..=0xCFFF => {} //ignore
            0x20..=0x21 => self.handle_pic_out(regs, qual),
            0xA0..=0xA1 => self.handle_pic_out(regs, qual),
            0x0070..=0x0071 => {} //ignore
            _ => {}
        }
    }

    pub fn handle_pic_in(&self, regs: &mut GuestRegisters, qual: QualIo) {
        match qual.port() {
            0x20 => {
                let v = match self.primary_read_sel {
                    ReadSel::IRR => self.primary_irr,
                    ReadSel::ISR => self.primary_isr,
                };
                regs.rax = v as u64;
            }
            0xA0 => {
                let v = match self.secondary_read_sel {
                    ReadSel::IRR => self.secondary_irr,
                    ReadSel::ISR => self.secondary_isr,
                };
                regs.rax = v as u64;
            }
            0x21 => match self.primary_phase {
                InitPhase::Uninitialized | InitPhase::Initialized => {
                    regs.rax = self.primary_mask as u64;
                }
                _ => {}
            },
            0xA1 => match self.secondary_phase {
                InitPhase::Uninitialized | InitPhase::Initialized => {
                    regs.rax = self.secondary_mask as u64;
                }
                _ => {}
            },
            _ => {}
        }
    }

    pub fn handle_pic_out(&mut self, regs: &mut GuestRegisters, qual: QualIo) {
        let pic = self;
        let dx = regs.rax as u8;
        match qual.port() {
            0x20 => match dx {
                0x11 => pic.primary_phase = InitPhase::Phase1,
                0x0A => pic.primary_read_sel = ReadSel::ISR,
                0x0B => pic.primary_read_sel = ReadSel::IRR,
                0x20 => {
                    pic.primary_isr = 0;
                }
                0x60..=0x67 => {
                    let irq = dx & 0x7;
                    pic.primary_isr &= !(1 << irq);
                }
                _ => panic!("Primary PIC command: {:#x}", dx),
            },
            0x21 => match pic.primary_phase {
                InitPhase::Uninitialized | InitPhase::Initialized => pic.primary_mask = dx,
                InitPhase::Phase1 => {
                    pic.primary_base = dx;
                    pic.primary_phase = InitPhase::Phase2;
                }
                InitPhase::Phase2 => {
                    pic.primary_phase = InitPhase::Phase3;
                }
                InitPhase::Phase3 => {
                    info!("Primary PIC Initialized");
                    pic.primary_phase = InitPhase::Initialized
                }
            },
            0xA0 => match dx {
                0x11 => pic.secondary_phase = InitPhase::Phase1,
                0x0A => pic.secondary_read_sel = ReadSel::ISR,
                0x0B => pic.secondary_read_sel = ReadSel::IRR,
                0x20 => {
                    pic.secondary_isr = 0;
                }
                0x60..=0x67 => {
                    let irq = dx & 0x7;
                    pic.secondary_isr &= !(1 << irq);
                }
                _ => panic!("Secondary PIC command: {:#x}", dx),
            },
            0xA1 => match pic.secondary_phase {
                InitPhase::Uninitialized | InitPhase::Initialized => pic.secondary_mask = dx,
                InitPhase::Phase1 => {
                    pic.secondary_base = dx;
                    pic.secondary_phase = InitPhase::Phase2;
                }
                InitPhase::Phase2 => {
                    pic.secondary_phase = InitPhase::Phase3;
                }
                InitPhase::Phase3 => {
                    info!("Secondary PIC Initialized");
                    pic.secondary_phase = InitPhase::Initialized
                }
            },
            _ => {}
        }
    }
}

pub struct IOBitmap {
    pub bitmap_a: PhysFrame,
    pub bitmap_b: PhysFrame,
}

impl IOBitmap {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Self {
        let bitmap_a = frame_allocator
            .allocate_frame()
            .expect("Failed to allocate I/O bitmap A");
        let bitmap_b = frame_allocator
            .allocate_frame()
            .expect("Failed to allocate I/O bitmap B");

        Self { bitmap_a, bitmap_b }
    }

    pub fn setup(&mut self) -> Result<(), &'static str> {
        let bitmap_a_addr = self.bitmap_a.start_address().as_u64() as usize;
        let bitmap_b_addr = self.bitmap_b.start_address().as_u64() as usize;

        unsafe {
            core::ptr::write_bytes(bitmap_a_addr as *mut u8, u8::MAX, 4096);
            core::ptr::write_bytes(bitmap_b_addr as *mut u8, u8::MAX, 4096);
        }

        self.set_io_ports(0x02F8..=0x03FF);
        self.set_io_ports(0x0040..=0x0047);

        vmwrite(vmcs::control::IO_BITMAP_A_ADDR_FULL, bitmap_a_addr as u64)?;
        vmwrite(vmcs::control::IO_BITMAP_B_ADDR_FULL, bitmap_b_addr as u64)?;

        Ok(())
    }

    pub fn set_io_ports(&mut self, ports: core::ops::RangeInclusive<u16>) {
        for port in ports {
            if port <= 0x7FFF {
                let byte_index = port as usize / 8;
                let bit_index = port as usize % 8;

                self.get_bitmap_a()[byte_index] &= !(1 << bit_index);
            } else {
                let adjusted_port = port - 0x8000;
                let byte_index = adjusted_port as usize / 8;
                let bit_index = adjusted_port as usize % 8;

                self.get_bitmap_b()[byte_index] &= !(1 << bit_index);
            }
        }
    }

    fn get_bitmap_a(&self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(self.bitmap_a.start_address().as_u64() as *mut u8, 4096)
        }
    }

    fn get_bitmap_b(&self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(self.bitmap_b.start_address().as_u64() as *mut u8, 4096)
        }
    }
}
