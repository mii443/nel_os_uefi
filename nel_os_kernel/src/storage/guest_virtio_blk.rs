use super::VirtioBlock;

const PCI_CONFIG_ADDRESS: u16 = 0x0cf8;
const PCI_CONFIG_DATA: u16 = 0x0cfc;
const PCI_BUS: u8 = 0;
const PCI_DEVICE: u8 = 4;
const PCI_FUNCTION: u8 = 0;
const HOST_BRIDGE_DEVICE: u8 = 0;
const ISA_BRIDGE_DEVICE: u8 = 1;
const DEFAULT_IO_BASE: u16 = 0xc000;
const IO_REGION_SIZE: u16 = 0x40;
const IRQ_LINE: u8 = 11;

const VIRTIO_VENDOR_ID: u16 = 0x1af4;
const VIRTIO_BLOCK_DEVICE_ID: u16 = 0x1001;
const QUEUE_SIZE: u16 = 128;
const SECTOR_SIZE: usize = 512;
const STATUS_DRIVER_OK: u8 = 4;
const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

pub trait GuestMemory {
    fn read_u8(&mut self, address: u64) -> Result<u8, &'static str>;
    fn write_u8(&mut self, address: u64, value: u8) -> Result<(), &'static str>;

    fn read_u16(&mut self, address: u64) -> Result<u16, &'static str> {
        let mut bytes = [0u8; 2];
        self.read_slice(address, &mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn read_u32(&mut self, address: u64) -> Result<u32, &'static str> {
        let mut bytes = [0u8; 4];
        self.read_slice(address, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn read_u64(&mut self, address: u64) -> Result<u64, &'static str> {
        let mut bytes = [0u8; 8];
        self.read_slice(address, &mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn write_u16(&mut self, address: u64, value: u16) -> Result<(), &'static str> {
        self.write_slice(address, &value.to_le_bytes())
    }

    fn write_u32(&mut self, address: u64, value: u32) -> Result<(), &'static str> {
        self.write_slice(address, &value.to_le_bytes())
    }

    fn read_slice(&mut self, address: u64, output: &mut [u8]) -> Result<(), &'static str> {
        for (offset, byte) in output.iter_mut().enumerate() {
            *byte = self.read_u8(address + offset as u64)?;
        }
        Ok(())
    }

    fn write_slice(&mut self, address: u64, input: &[u8]) -> Result<(), &'static str> {
        for (offset, &byte) in input.iter().enumerate() {
            self.write_u8(address + offset as u64, byte)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Descriptor {
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigDevice {
    HostBridge,
    IsaBridge,
    PmBridge,
    VirtioBlock,
}

pub struct GuestVirtioBlock {
    config_address: u32,
    command: u16,
    io_base: u16,
    bar_probe: bool,
    guest_features: u32,
    queue_address: u32,
    queue_select: u16,
    device_status: u8,
    isr_status: u8,
    last_available_index: u16,
    interrupt_pending: bool,
}

impl GuestVirtioBlock {
    pub const fn new() -> Self {
        Self {
            config_address: 0,
            command: 0,
            io_base: DEFAULT_IO_BASE,
            bar_probe: false,
            guest_features: 0,
            queue_address: 0,
            queue_select: 0,
            device_status: 0,
            isr_status: 0,
            last_available_index: 0,
            interrupt_pending: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn handles_port(&self, port: u16) -> bool {
        (PCI_CONFIG_ADDRESS..=PCI_CONFIG_ADDRESS + 3).contains(&port)
            || ((PCI_CONFIG_DATA..=PCI_CONFIG_DATA + 3).contains(&port)
                && self.selected_config_device().is_some())
            || (self.command & 1 != 0
                && (self.io_base..self.io_base + IO_REGION_SIZE).contains(&port))
    }

    pub fn write_config_address(&mut self, port: u16, size: u8, value: u32) {
        if (PCI_CONFIG_ADDRESS..=PCI_CONFIG_ADDRESS + 3).contains(&port) {
            merge(
                &mut self.config_address,
                port - PCI_CONFIG_ADDRESS,
                size,
                value,
            );
        }
    }

    pub fn interrupt_level(&self) -> (u8, bool) {
        (IRQ_LINE, self.interrupt_pending)
    }

    pub fn io_in(&mut self, port: u16, size: u8, capacity_sectors: u64) -> u32 {
        if (PCI_CONFIG_ADDRESS..=PCI_CONFIG_ADDRESS + 3).contains(&port) {
            return extract(self.config_address, port - PCI_CONFIG_ADDRESS, size);
        }
        if (PCI_CONFIG_DATA..=PCI_CONFIG_DATA + 3).contains(&port)
            && self.selected_config_device().is_some()
        {
            let offset = ((self.config_address & 0xfc) as u16) + port - PCI_CONFIG_DATA;
            return self.config_read(self.selected_config_device().unwrap(), offset, size);
        }
        if (self.io_base..self.io_base + IO_REGION_SIZE).contains(&port) {
            return self.device_read(port - self.io_base, size, capacity_sectors);
        }
        width_mask(size)
    }

    pub fn io_out<M: GuestMemory>(
        &mut self,
        port: u16,
        size: u8,
        value: u32,
        memory: &mut M,
        backend: &mut VirtioBlock,
    ) -> Result<(), &'static str> {
        if (PCI_CONFIG_ADDRESS..=PCI_CONFIG_ADDRESS + 3).contains(&port) {
            self.write_config_address(port, size, value);
            return Ok(());
        }
        if (PCI_CONFIG_DATA..=PCI_CONFIG_DATA + 3).contains(&port)
            && self.selected_config_device().is_some()
        {
            let offset = ((self.config_address & 0xfc) as u16) + port - PCI_CONFIG_DATA;
            self.config_write(self.selected_config_device().unwrap(), offset, size, value);
            return Ok(());
        }
        if (self.io_base..self.io_base + IO_REGION_SIZE).contains(&port) {
            self.device_write(port - self.io_base, size, value, memory, backend)?;
        }
        Ok(())
    }

    fn selected_config_device(&self) -> Option<ConfigDevice> {
        if self.config_address & (1 << 31) == 0 {
            return None;
        }
        let selected = self.config_address & 0x00ff_ff00;
        let bdf = |device: u8, function: u8| {
            ((PCI_BUS as u32) << 16) | ((device as u32) << 11) | ((function as u32) << 8)
        };
        match selected {
            value if value == bdf(PCI_DEVICE, PCI_FUNCTION) => Some(ConfigDevice::VirtioBlock),
            value if value == bdf(HOST_BRIDGE_DEVICE, 0) => Some(ConfigDevice::HostBridge),
            value if value == bdf(ISA_BRIDGE_DEVICE, 0) => Some(ConfigDevice::IsaBridge),
            value if value == bdf(ISA_BRIDGE_DEVICE, 3) => Some(ConfigDevice::PmBridge),
            _ => None,
        }
    }

    fn config_read(&self, device: ConfigDevice, offset: u16, size: u8) -> u32 {
        let aligned = offset & !3;
        let value = match (device, aligned) {
            // Present the conventional i440FX/PIIX chipset that OVMF supports
            // through PCI configuration mechanism #1. These functions are
            // synthetic; no physical host bridge is exposed to the guest.
            (ConfigDevice::HostBridge, 0x00) => 0x1237_8086,
            (ConfigDevice::HostBridge, 0x04) => 0x0000_0006,
            (ConfigDevice::HostBridge, 0x08) => 0x0600_0000,
            (ConfigDevice::HostBridge, 0x0c) => 0x0000_0000,
            (ConfigDevice::IsaBridge, 0x00) => 0x7000_8086,
            (ConfigDevice::IsaBridge, 0x04) => 0x0200_0007,
            (ConfigDevice::IsaBridge, 0x08) => 0x0601_0000,
            (ConfigDevice::IsaBridge, 0x0c) => 0x0080_0000,
            (ConfigDevice::PmBridge, 0x00) => 0x7113_8086,
            (ConfigDevice::PmBridge, 0x04) => 0x0280_0001,
            (ConfigDevice::PmBridge, 0x08) => 0x0680_0003,
            (ConfigDevice::PmBridge, 0x0c) => 0x0000_0000,
            (ConfigDevice::PmBridge, 0x40) => 0x0000_b001,
            (ConfigDevice::PmBridge, 0x80) => 0x0000_0001,
            (ConfigDevice::VirtioBlock, 0x00) => {
                u32::from(VIRTIO_VENDOR_ID) | (u32::from(VIRTIO_BLOCK_DEVICE_ID) << 16)
            }
            (ConfigDevice::VirtioBlock, 0x04) => u32::from(self.command),
            (ConfigDevice::VirtioBlock, 0x08) => 0x0180_0000,
            (ConfigDevice::VirtioBlock, 0x0c) => 0,
            (ConfigDevice::VirtioBlock, 0x10) => {
                if self.bar_probe {
                    0xffff_ffc1
                } else {
                    u32::from(self.io_base) | 1
                }
            }
            (ConfigDevice::VirtioBlock, 0x2c) => u32::from(VIRTIO_VENDOR_ID) | (2 << 16),
            (ConfigDevice::VirtioBlock, 0x3c) => u32::from(IRQ_LINE) | (1 << 8),
            _ => u32::MAX,
        };
        extract(value, offset - aligned, size)
    }

    fn config_write(&mut self, device: ConfigDevice, offset: u16, size: u8, value: u32) {
        if device != ConfigDevice::VirtioBlock {
            return;
        }
        let aligned = offset & !3;
        if aligned == 0x04 {
            let mut command = u32::from(self.command);
            merge(&mut command, offset - aligned, size, value);
            self.command = command as u16 & 0x0407;
        } else if aligned == 0x10 && offset == 0x10 && size == 4 {
            if value == u32::MAX {
                self.bar_probe = true;
            } else {
                self.bar_probe = false;
                let base = value & 0xffff_ffc0;
                if base <= u16::MAX as u32 {
                    self.io_base = base as u16;
                }
            }
        }
    }

    fn device_read(&mut self, offset: u16, size: u8, capacity_sectors: u64) -> u32 {
        if (20..28).contains(&offset) && u16::from(size) <= 28 - offset {
            let shift = u32::from(offset - 20) * 8;
            return ((capacity_sectors >> shift) as u32) & width_mask(size);
        }
        let value = match (offset, size) {
            (0, 4) => 0,
            (4, 4) => self.guest_features,
            (8, 4) => self.queue_address,
            (12, 2) => {
                if self.queue_select == 0 {
                    u32::from(QUEUE_SIZE)
                } else {
                    0
                }
            }
            (14, 2) => u32::from(self.queue_select),
            (18, 1) => u32::from(self.device_status),
            (19, 1) => {
                let value = self.isr_status;
                self.isr_status = 0;
                self.interrupt_pending = false;
                u32::from(value)
            }
            (_, 1) => u8::MAX as u32,
            (_, 2) => u16::MAX as u32,
            _ => u32::MAX,
        };
        value & width_mask(size)
    }

    fn device_write<M: GuestMemory>(
        &mut self,
        offset: u16,
        size: u8,
        value: u32,
        memory: &mut M,
        backend: &mut VirtioBlock,
    ) -> Result<(), &'static str> {
        match (offset, size) {
            (4, 4) => self.guest_features = value,
            (8, 4) => self.queue_address = value,
            (14, 2) => self.queue_select = value as u16,
            (16, 2) if value as u16 == 0 => self.process_queue(memory, backend)?,
            (18, 1) => {
                self.device_status = value as u8;
                if self.device_status == 0 {
                    self.queue_address = 0;
                    self.last_available_index = 0;
                    self.isr_status = 0;
                    self.interrupt_pending = false;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn process_queue<M: GuestMemory>(
        &mut self,
        memory: &mut M,
        backend: &mut VirtioBlock,
    ) -> Result<(), &'static str> {
        if self.device_status & STATUS_DRIVER_OK == 0 || self.queue_address == 0 {
            return Ok(());
        }
        let base = u64::from(self.queue_address) << 12;
        let available = base + u64::from(QUEUE_SIZE) * 16;
        let used = align_up(available + 6 + u64::from(QUEUE_SIZE) * 2, 4096);
        let available_index = memory.read_u16(available + 2)?;
        while self.last_available_index != available_index {
            let slot = self.last_available_index % QUEUE_SIZE;
            let head = memory.read_u16(available + 4 + u64::from(slot) * 2)?;
            let written = self.process_request(base, head, memory, backend)?;
            let used_index = memory.read_u16(used + 2)?;
            let used_slot = used_index % QUEUE_SIZE;
            let element = used + 4 + u64::from(used_slot) * 8;
            memory.write_u32(element, u32::from(head))?;
            memory.write_u32(element + 4, written)?;
            memory.write_u16(used + 2, used_index.wrapping_add(1))?;
            self.last_available_index = self.last_available_index.wrapping_add(1);
        }
        self.isr_status |= 1;
        self.interrupt_pending = true;
        Ok(())
    }

    fn process_request<M: GuestMemory>(
        &mut self,
        descriptor_base: u64,
        head: u16,
        memory: &mut M,
        backend: &mut VirtioBlock,
    ) -> Result<u32, &'static str> {
        let header = read_descriptor(descriptor_base, head, memory)?;
        if header.length < 16 || header.flags & DESC_F_WRITE != 0 || header.flags & DESC_F_NEXT == 0
        {
            return Err("guest virtio-blk request has an invalid header descriptor");
        }
        let request_type = memory.read_u32(header.address)?;
        let request_supported = matches!(request_type, 0 | 1);
        let mut sector = memory.read_u64(header.address + 8)?;
        let mut descriptor_id = header.next;
        let mut transferred = 0u32;
        let mut traversed = 1u16;
        let mut sector_buffer = [0u8; SECTOR_SIZE];

        loop {
            if traversed >= QUEUE_SIZE {
                return Err("guest virtio-blk descriptor chain is cyclic");
            }
            let descriptor = read_descriptor(descriptor_base, descriptor_id, memory)?;
            traversed += 1;
            let is_status = descriptor.flags & DESC_F_NEXT == 0;
            if is_status {
                if descriptor.length < 1 || descriptor.flags & DESC_F_WRITE == 0 {
                    return Err("guest virtio-blk request has an invalid status descriptor");
                }
                memory.write_u8(descriptor.address, if request_supported { 0 } else { 2 })?;
                return Ok(if request_type == 0 {
                    transferred.saturating_add(1)
                } else {
                    1
                });
            }
            if descriptor.length as usize % SECTOR_SIZE != 0 {
                return Err("guest virtio-blk data descriptor is not sector aligned");
            }
            let sectors = descriptor.length as usize / SECTOR_SIZE;
            for index in 0..sectors {
                let address = descriptor.address + (index * SECTOR_SIZE) as u64;
                match request_type {
                    0 if descriptor.flags & DESC_F_WRITE != 0 => {
                        backend.read_sector(sector, &mut sector_buffer)?;
                        memory.write_slice(address, &sector_buffer)?;
                    }
                    1 if descriptor.flags & DESC_F_WRITE == 0 => {
                        memory.read_slice(address, &mut sector_buffer)?;
                        backend.write_sector(sector, &sector_buffer)?;
                    }
                    0 | 1 => return Err("guest virtio-blk data descriptor has invalid flags"),
                    _ => {}
                }
                sector = sector
                    .checked_add(1)
                    .ok_or("guest virtio-blk sector overflow")?;
                transferred = transferred.saturating_add(SECTOR_SIZE as u32);
            }
            descriptor_id = descriptor.next;
        }
    }
}

fn read_descriptor<M: GuestMemory>(
    base: u64,
    id: u16,
    memory: &mut M,
) -> Result<Descriptor, &'static str> {
    if id >= QUEUE_SIZE {
        return Err("guest virtio-blk descriptor ID is out of range");
    }
    let address = base + u64::from(id) * 16;
    Ok(Descriptor {
        address: memory.read_u64(address)?,
        length: memory.read_u32(address + 8)?,
        flags: memory.read_u16(address + 12)?,
        next: memory.read_u16(address + 14)?,
    })
}

const fn align_up(value: u64, alignment: u64) -> u64 {
    (value + alignment - 1) & !(alignment - 1)
}

fn width_mask(size: u8) -> u32 {
    match size {
        1 => u8::MAX as u32,
        2 => u16::MAX as u32,
        _ => u32::MAX,
    }
}

fn extract(value: u32, byte_offset: u16, size: u8) -> u32 {
    (value >> (u32::from(byte_offset) * 8)) & width_mask(size)
}

fn merge(target: &mut u32, byte_offset: u16, size: u8, value: u32) {
    let shift = u32::from(byte_offset) * 8;
    let mask = width_mask(size) << shift;
    *target = (*target & !mask) | ((value << shift) & mask);
}

impl Default for GuestVirtioBlock {
    fn default() -> Self {
        Self::new()
    }
}
