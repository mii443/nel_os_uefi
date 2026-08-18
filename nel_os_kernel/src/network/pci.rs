use x86_64::instructions::port::Port;

const CONFIG_ADDRESS: u16 = 0x0cf8;
const CONFIG_DATA: u16 = 0x0cfc;

pub const VIRTIO_VENDOR_ID: u16 = 0x1af4;
pub const VIRTIO_NET_LEGACY_DEVICE_ID: u16 = 0x1000;
pub const VIRTIO_NET_MODERN_DEVICE_ID: u16 = 0x1041;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    pub(crate) fn config_address(self, offset: u8) -> u32 {
        0x8000_0000
            | (self.bus as u32) << 16
            | (self.device as u32) << 11
            | (self.function as u32) << 8
            | (offset as u32 & 0xfc)
    }

    pub fn read_u32(self, offset: u8) -> u32 {
        unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(self.config_address(offset));
            Port::<u32>::new(CONFIG_DATA).read()
        }
    }

    pub fn read_u16(self, offset: u8) -> u16 {
        let shift = (offset & 2) as u32 * 8;
        (self.read_u32(offset) >> shift) as u16
    }

    pub fn read_u8(self, offset: u8) -> u8 {
        let shift = (offset & 3) as u32 * 8;
        (self.read_u32(offset) >> shift) as u8
    }

    pub fn write_u16(self, offset: u8, value: u16) {
        let aligned = offset & 0xfc;
        let shift = (offset & 2) as u32 * 8;
        let old = self.read_u32(aligned);
        let new = (old & !((u16::MAX as u32) << shift)) | ((value as u32) << shift);
        unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(self.config_address(aligned));
            Port::<u32>::new(CONFIG_DATA).write(new);
        }
    }

    pub fn write_u32(self, offset: u8, value: u32) {
        unsafe {
            Port::<u32>::new(CONFIG_ADDRESS).write(self.config_address(offset));
            Port::<u32>::new(CONFIG_DATA).write(value);
        }
    }
}

pub fn find_legacy_virtio_net() -> Option<PciAddress> {
    find_nth_legacy_virtio_net(0)
}

pub fn find_nth_legacy_virtio_net(mut index: usize) -> Option<PciAddress> {
    find_nth_virtio_net_matching(&mut index, false)
}

pub fn find_nth_virtio_net(mut index: usize) -> Option<PciAddress> {
    find_nth_virtio_net_matching(&mut index, true)
}

fn find_nth_virtio_net_matching(index: &mut usize, include_modern: bool) -> Option<PciAddress> {
    for bus in 0..=u8::MAX {
        for device in 0..32u8 {
            let first = PciAddress {
                bus,
                device,
                function: 0,
            };
            if first.read_u16(0) == u16::MAX {
                continue;
            }

            let functions = if first.read_u8(0x0e) & 0x80 != 0 {
                8
            } else {
                1
            };
            for function in 0..functions {
                let address = PciAddress {
                    bus,
                    device,
                    function,
                };
                let device_id = address.read_u16(2);
                if address.read_u16(0) == VIRTIO_VENDOR_ID
                    && (device_id == VIRTIO_NET_LEGACY_DEVICE_ID
                        || include_modern && device_id == VIRTIO_NET_MODERN_DEVICE_ID)
                {
                    if *index == 0 {
                        return Some(address);
                    }
                    *index -= 1;
                }
            }
        }
    }
    None
}

pub fn bus_has_device(bus: u8) -> bool {
    (0..32u8).any(|device| {
        PciAddress {
            bus,
            device,
            function: 0,
        }
        .read_u16(0)
            != u16::MAX
    })
}

pub fn io_bar(address: PciAddress) -> Option<u16> {
    // A transitional virtio device normally puts the legacy registers in
    // BAR0, but scan every ordinary BAR rather than relying on that detail.
    for offset in (0x10..=0x24).step_by(4) {
        let bar = address.read_u32(offset);
        if bar & 1 != 0 {
            let base = bar & !3;
            if base != 0 && base <= u16::MAX as u32 {
                return Some(base as u16);
            }
        }
    }
    None
}

pub fn enable_io_bus_mastering(address: PciAddress) {
    const IO_SPACE: u16 = 1 << 0;
    const BUS_MASTER: u16 = 1 << 2;
    const INTERRUPT_DISABLE: u16 = 1 << 10;

    let command = address.read_u16(0x04);
    address.write_u16(0x04, command | IO_SPACE | BUS_MASTER | INTERRUPT_DISABLE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_mechanism_one_address() {
        let address = PciAddress {
            bus: 2,
            device: 3,
            function: 4,
        };
        assert_eq!(address.config_address(0x13), 0x8002_1c10);
    }
}
