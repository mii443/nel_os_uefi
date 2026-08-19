mod guest_virtio_blk;
mod virtio_blk;

pub use guest_virtio_blk::{GuestMemory, GuestVirtioBlock};
pub use virtio_blk::VirtioBlock;
