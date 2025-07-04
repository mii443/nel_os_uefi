#![no_std]

use crate::{gop::FrameBuffer, memory::UsableMemory};

pub mod gop;
pub mod memory;

pub struct BootInfo {
    pub usable_memory: UsableMemory,
    pub frame_buffer: FrameBuffer,
    pub rsdp: u64,
}
