use super::qual::QualIo;
use crate::{info, vmm::x86_64::intel::register::GuestRegisters};

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

pub struct PIC {
    pub primary_mask: u8,
    pub secondary_mask: u8,
    pub primary_phase: InitPhase,
    pub secondary_phase: InitPhase,
    pub primary_base: u8,
    pub secondary_base: u8,
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
                0x60..=0x67 => {}
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
                0x60..=0x67 => {}
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
