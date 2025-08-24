#![allow(non_snake_case)]

use core::convert::TryFrom;
use core::fmt::Debug;

use modular_bitfield::prelude::{B1, B16, B3, B32, B4, B9};
use modular_bitfield::{bitfield, Specifier};

#[repr(u8)]
#[derive(Specifier, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    MovTo = 0,
    MovFrom = 1,
    Clts = 2,
    Lmsw = 3,
}

impl TryFrom<u8> for AccessType {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(AccessType::MovTo),
            1 => Ok(AccessType::MovFrom),
            2 => Ok(AccessType::Clts),
            3 => Ok(AccessType::Lmsw),
            _ => Err("Invalid AccessType value"),
        }
    }
}

#[repr(u8)]
#[derive(Specifier, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmswOperandType {
    Reg = 0,
    Mem = 1,
}

impl TryFrom<u8> for LmswOperandType {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(LmswOperandType::Reg),
            1 => Ok(LmswOperandType::Mem),
            _ => Err("Invalid LmswOperandType value"),
        }
    }
}

#[repr(u8)]
#[derive(Specifier, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Register {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl TryFrom<u8> for Register {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Register::Rax),
            1 => Ok(Register::Rcx),
            2 => Ok(Register::Rdx),
            3 => Ok(Register::Rbx),
            4 => Ok(Register::Rsp),
            5 => Ok(Register::Rbp),
            6 => Ok(Register::Rsi),
            7 => Ok(Register::Rdi),
            8 => Ok(Register::R8),
            9 => Ok(Register::R9),
            10 => Ok(Register::R10),
            11 => Ok(Register::R11),
            12 => Ok(Register::R12),
            13 => Ok(Register::R13),
            14 => Ok(Register::R14),
            15 => Ok(Register::R15),
            _ => Err("Invalid Register value"),
        }
    }
}

#[bitfield]
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub struct QualCr {
    pub index: B4,
    #[bits = 2]
    pub access_type: AccessType,
    #[bits = 1]
    pub lmsw_operand_type: LmswOperandType,
    _reserved1: B1,
    #[bits = 4]
    pub register: Register,
    _reserved2: B4,
    pub lmsw_source: B16,
    _reseved3: B32,
}

#[bitfield]
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub struct QualIo {
    pub size: B3,
    pub direction: B1,
    pub string: B1,
    pub rep: B1,
    pub operand_encoding: B1,
    _reserved1: B9,
    pub port: B16,
    _reserved2: B32,
}
