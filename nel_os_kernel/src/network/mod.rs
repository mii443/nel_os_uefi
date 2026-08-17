//! Host-only networking for the outer hypervisor.
//!
//! The module owns a physical legacy virtio-net PCI function. Nothing in this
//! module is connected to guest address translation or guest PCI emulation.

mod pci;
mod stack;
mod virtio_net;

pub use stack::CONTROL_PORT;
pub use virtio_net::VirtioNet;
