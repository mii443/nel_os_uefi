#![no_std]

use crate::{gop::FrameBuffer, memory::UsableMemory};

pub mod gop;
pub mod memory;

pub struct BootInfo {
    pub usable_memory: UsableMemory,
    pub frame_buffer: FrameBuffer,
    pub rsdp: u64,
    pub bzimage_addr: u64,
    pub bzimage_size: u64,
    pub rootfs_addr: u64,
    pub rootfs_size: u64,
}
