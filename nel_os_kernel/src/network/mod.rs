//! Networking for the outer hypervisor and the VM-0 assigned PCI function.

mod management;
mod passthrough;
mod pci;
mod stack;
mod virtio_net;

pub use crate::management::ManagementCommand;
pub(crate) use management::ConnectionId;
pub use passthrough::{PassthroughDescriptor, PassthroughNic};
pub use stack::{CONTROL_PORT, Ipv4Config};
pub use virtio_net::VirtioNet;
