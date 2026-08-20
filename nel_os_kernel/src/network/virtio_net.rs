use core::{
    arch::x86_64::_rdrand64_step,
    ptr::{read_volatile, write_volatile},
    slice,
    sync::atomic::{Ordering, fence},
};

use raw_cpuid::cpuid;
use x86_64::{
    instructions::port::Port,
    structures::paging::{FrameAllocator, PageSize, PhysFrame, Size4KiB},
};

use crate::{info, memory::bitmap::BitmapMemoryTable, time, warn};

use super::{
    ManagementCommand,
    pci::{self, PciAddress},
    stack::{CONTROL_PORT, NetworkStack},
};

const DEVICE_FEATURES: u16 = 0;
const GUEST_FEATURES: u16 = 4;
const QUEUE_ADDRESS: u16 = 8;
const QUEUE_SIZE: u16 = 12;
const QUEUE_SELECT: u16 = 14;
const QUEUE_NOTIFY: u16 = 16;
const DEVICE_STATUS: u16 = 18;
const ISR_STATUS: u16 = 19;
const DEVICE_CONFIG: u16 = 20;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 128;

const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;
const RX_BUFFER_COUNT: usize = 32;
const BUFFER_SIZE: usize = Size4KiB::SIZE as usize;
const VIRTIO_NET_HEADER_LEN: usize = 10;
const MAX_ETHERNET_FRAME: usize = 2048;
const MIN_ETHERNET_FRAME: usize = 60;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Descriptor {
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UsedElement {
    id: u32,
    length: u32,
}

#[derive(Clone, Copy)]
struct UsedBuffer {
    id: u16,
    length: u32,
}

struct VirtQueue {
    index: u16,
    size: u16,
    _memory: PhysFrame<Size4KiB>,
    _memory_pages: usize,
    descriptor: *mut Descriptor,
    available: *mut u8,
    used: *mut u8,
    last_used_index: u16,
}

impl VirtQueue {
    fn create(
        io_base: u16,
        index: u16,
        allocator: &mut BitmapMemoryTable,
    ) -> Result<Self, &'static str> {
        io_write_u16(io_base + QUEUE_SELECT, index);
        let size = io_read_u16(io_base + QUEUE_SIZE);
        if size == 0 || !size.is_power_of_two() {
            return Err("virtio-net exposed an invalid queue size");
        }

        let available_offset = core::mem::size_of::<Descriptor>() * size as usize;
        let used_offset = align_up(available_offset + 6 + 2 * size as usize, BUFFER_SIZE);
        let bytes = used_offset + 6 + core::mem::size_of::<UsedElement>() * size as usize;
        let pages = bytes.div_ceil(BUFFER_SIZE);
        let memory = allocator
            .allocate_contiguous_frames(pages, 1)
            .ok_or("no contiguous DMA memory for a virtio-net queue")?;
        let physical = memory.start_address().as_u64();
        if physical >> 12 > u32::MAX as u64 {
            return Err("virtio legacy queue memory is above its 44-bit DMA limit");
        }

        unsafe {
            core::ptr::write_bytes(physical as *mut u8, 0, pages * BUFFER_SIZE);
        }
        let queue = Self {
            index,
            size,
            _memory: memory,
            _memory_pages: pages,
            descriptor: physical as *mut Descriptor,
            available: (physical as usize + available_offset) as *mut u8,
            used: (physical as usize + used_offset) as *mut u8,
            last_used_index: 0,
        };
        queue.write_available_flags(VIRTQ_AVAIL_F_NO_INTERRUPT);

        io_write_u32(io_base + QUEUE_ADDRESS, (physical >> 12) as u32);
        Ok(queue)
    }

    fn set_descriptor(&mut self, id: u16, address: u64, length: u32, flags: u16) {
        debug_assert!(id < self.size);
        unsafe {
            write_volatile(
                self.descriptor.add(id as usize),
                Descriptor {
                    address,
                    length,
                    flags,
                    next: 0,
                },
            );
        }
    }

    fn make_available(&mut self, id: u16) {
        let available_index = self.read_available_index();
        let slot = available_index % self.size;
        unsafe {
            write_volatile(self.available.add(4 + slot as usize * 2) as *mut u16, id);
        }
        fence(Ordering::Release);
        self.write_available_index(available_index.wrapping_add(1));
    }

    fn pop_used(&mut self) -> Option<UsedBuffer> {
        let device_index = unsafe { read_volatile(self.used.add(2) as *const u16) };
        fence(Ordering::Acquire);
        if self.last_used_index == device_index {
            return None;
        }

        let slot = self.last_used_index % self.size;
        let element = unsafe {
            read_volatile(
                self.used
                    .add(4 + slot as usize * core::mem::size_of::<UsedElement>())
                    as *const UsedElement,
            )
        };
        self.last_used_index = self.last_used_index.wrapping_add(1);
        if element.id >= self.size as u32 {
            return Some(UsedBuffer {
                id: u16::MAX,
                length: element.length,
            });
        }
        Some(UsedBuffer {
            id: element.id as u16,
            length: element.length,
        })
    }

    fn read_available_index(&self) -> u16 {
        unsafe { read_volatile(self.available.add(2) as *const u16) }
    }

    fn write_available_index(&self, value: u16) {
        unsafe { write_volatile(self.available.add(2) as *mut u16, value) }
    }

    fn write_available_flags(&self, value: u16) {
        unsafe { write_volatile(self.available as *mut u16, value) }
    }

    fn notify(&self, io_base: u16) {
        io_write_u16(io_base + QUEUE_NOTIFY, self.index);
    }
}

// Queue and buffer pointers refer to reserved, identity-mapped physical pages.
// They are never freed and the driver is only polled by the bootstrap CPU.
unsafe impl Send for VirtQueue {}

pub struct VirtioNet {
    pci_address: PciAddress,
    io_base: u16,
    receive: VirtQueue,
    transmit: VirtQueue,
    receive_buffers: [u64; RX_BUFFER_COUNT],
    receive_buffer_count: usize,
    transmit_buffer: u64,
    transmit_in_flight: bool,
    pending_transmit: [u8; MAX_ETHERNET_FRAME],
    pending_transmit_len: usize,
    dropped_transmits: u64,
    stack: NetworkStack,
}

impl VirtioNet {
    pub fn probe(allocator: &mut BitmapMemoryTable) -> Result<Self, &'static str> {
        let pci_address = pci::find_legacy_virtio_net()
            .ok_or("no transitional virtio-net PCI device was found")?;
        let io_base = pci::io_bar(pci_address).ok_or("virtio-net has no legacy I/O BAR")?;
        pci::enable_io_bus_mastering(pci_address);

        io_write_u8(io_base + DEVICE_STATUS, 0);
        io_write_u8(io_base + DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        let features = io_read_u32(io_base + DEVICE_FEATURES);
        if features & VIRTIO_NET_F_MAC == 0 {
            fail_device(io_base);
            return Err("virtio-net does not provide a stable MAC address");
        }
        // Deliberately negotiate neither checksum offload nor merged receive
        // buffers, keeping every packet in one buffer behind a 10-byte header.
        io_write_u32(io_base + GUEST_FEATURES, VIRTIO_NET_F_MAC);

        let mut mac = [0u8; 6];
        for (index, byte) in mac.iter_mut().enumerate() {
            *byte = io_read_u8(io_base + DEVICE_CONFIG + index as u16);
        }

        let mut receive = VirtQueue::create(io_base, RX_QUEUE, allocator).inspect_err(|_| {
            fail_device(io_base);
        })?;
        if (receive.size as usize) < RX_BUFFER_COUNT {
            fail_device(io_base);
            return Err("virtio-net receive queue is too small");
        }
        let transmit = VirtQueue::create(io_base, TX_QUEUE, allocator).inspect_err(|_| {
            fail_device(io_base);
        })?;

        let mut receive_buffers = [0u64; RX_BUFFER_COUNT];
        for (id, address) in receive_buffers.iter_mut().enumerate() {
            let frame = allocator
                .allocate_frame()
                .ok_or("no DMA frame for a virtio-net receive buffer")?;
            *address = frame.start_address().as_u64();
            unsafe {
                core::ptr::write_bytes(*address as *mut u8, 0, BUFFER_SIZE);
            }
            receive.set_descriptor(id as u16, *address, BUFFER_SIZE as u32, VIRTQ_DESC_F_WRITE);
            receive.make_available(id as u16);
        }

        let transmit_frame = allocator
            .allocate_frame()
            .ok_or("no DMA frame for the virtio-net transmit buffer")?;
        let transmit_buffer = transmit_frame.start_address().as_u64();
        unsafe {
            core::ptr::write_bytes(transmit_buffer as *mut u8, 0, BUFFER_SIZE);
        }

        io_write_u8(
            io_base + DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK,
        );
        if io_read_u8(io_base + DEVICE_STATUS) & STATUS_FAILED != 0 {
            return Err("virtio-net rejected driver initialization");
        }
        fence(Ordering::SeqCst);
        receive.notify(io_base);

        let mut stack = NetworkStack::new(mac);
        if let Some(token) = generate_management_token() {
            stack.require_management_auth(token);
            let token = unsafe { core::str::from_utf8_unchecked(&token) };
            info!("Management authentication token: {}", token);
        } else {
            stack.disable_remote_management();
            warn!("RDRAND unavailable; remote TCP/UDP management is disabled");
        }

        let device = Self {
            pci_address,
            io_base,
            receive,
            transmit,
            receive_buffers,
            receive_buffer_count: RX_BUFFER_COUNT,
            transmit_buffer,
            transmit_in_flight: false,
            pending_transmit: [0; MAX_ETHERNET_FRAME],
            pending_transmit_len: 0,
            dropped_transmits: 0,
            stack,
        };
        info!(
            "Host-only virtio-net at {:02x}:{:02x}.{} I/O {:#x}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            device.pci_address.bus,
            device.pci_address.device,
            device.pci_address.function,
            device.io_base,
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        );
        info!(
            "Hypervisor DHCP client started; VM control is UDP/TCP port {}",
            CONTROL_PORT
        );
        Ok(device)
    }

    pub fn poll(&mut self) -> Result<(), &'static str> {
        // Reading ISR acknowledges any stale legacy interrupt even though PCI
        // INTx and virtqueue interrupts are disabled for this polling driver.
        let _ = io_read_u8(self.io_base + ISR_STATUS);
        self.reap_transmit()?;
        self.flush_pending_transmit()?;

        // Do not consume protocol state when both bounded transmit slots are
        // occupied. In particular, dropping a TCP ACK here would stall an
        // otherwise healthy management session until retransmission.
        if self.transmit_in_flight && self.pending_transmit_len != 0 {
            return Ok(());
        }

        let now = time::get_ticks();
        let previous_config = self.stack.ipv4_config();
        let mut response = [0u8; MAX_ETHERNET_FRAME];
        if let Some(length) = self.stack.poll(now, &mut response) {
            if self.transmit_or_queue(&response[..length]).is_err() {
                self.dropped_transmits = self.dropped_transmits.saturating_add(1);
            }
        }

        let mut receive_buffers_requeued = false;
        if (!self.transmit_in_flight || self.pending_transmit_len == 0)
            && let Some(used) = self.receive.pop_used()
        {
            let id = used.id as usize;
            if id >= self.receive_buffer_count {
                return Err("virtio-net returned an invalid receive descriptor");
            }
            let used_len = used.length as usize;
            if used_len >= VIRTIO_NET_HEADER_LEN && used_len <= BUFFER_SIZE {
                let frame = unsafe {
                    slice::from_raw_parts(
                        (self.receive_buffers[id] as usize + VIRTIO_NET_HEADER_LEN) as *const u8,
                        used_len - VIRTIO_NET_HEADER_LEN,
                    )
                };
                if let Some(length) = self.stack.handle_frame(frame, &mut response, now) {
                    if self.transmit_or_queue(&response[..length]).is_err() {
                        self.dropped_transmits = self.dropped_transmits.saturating_add(1);
                    }
                }
            }

            fence(Ordering::Release);
            self.receive.make_available(used.id);
            receive_buffers_requeued = true;
        }
        if receive_buffers_requeued {
            fence(Ordering::SeqCst);
            self.receive.notify(self.io_base);
        }

        let current_config = self.stack.ipv4_config();
        if previous_config != current_config {
            if let Some(config) = current_config {
                info!(
                    "DHCP lease: {}.{}.{}.{}/{}.{}.{}.{}, router {}.{}.{}.{}, {} seconds",
                    config.address[0],
                    config.address[1],
                    config.address[2],
                    config.address[3],
                    config.subnet_mask[0],
                    config.subnet_mask[1],
                    config.subnet_mask[2],
                    config.subnet_mask[3],
                    config.router[0],
                    config.router[1],
                    config.router[2],
                    config.router[3],
                    config.lease_seconds
                );
                info!(
                    "Management shell is listening on TCP port {}; UDP `start` remains available",
                    CONTROL_PORT
                );
            } else {
                warn!("DHCP lease expired; restarting address discovery");
            }
        }
        Ok(())
    }

    pub fn take_start_request(&mut self) -> bool {
        self.stack.take_start_request()
    }

    pub fn take_management_command(&mut self) -> Option<(super::ConnectionId, ManagementCommand)> {
        self.stack.take_management_command()
    }

    pub fn write_management(&mut self, id: super::ConnectionId, bytes: &[u8]) -> usize {
        self.stack.write_management(id, bytes)
    }

    pub fn management_prompt(&mut self, id: super::ConnectionId) {
        self.stack.management_prompt(id);
    }

    pub fn notify_management_detach_or_close(
        &mut self,
        id: super::ConnectionId,
        notice: &[u8],
    ) -> bool {
        self.stack.notify_management_detach_or_close(id, notice)
    }

    pub fn write_management_help(&mut self, id: super::ConnectionId) {
        self.stack.write_management_help(id);
    }

    pub fn request_management_close(&mut self, id: super::ConnectionId) {
        self.stack.request_management_close(id);
    }

    pub fn set_serial_attached(&mut self, id: super::ConnectionId, attached: bool) {
        self.stack.set_serial_attached(id, attached);
    }

    pub fn serial_attached(&self, id: super::ConnectionId) -> bool {
        self.stack.serial_attached(id)
    }

    pub fn take_serial_input(&mut self, id: super::ConnectionId, output: &mut [u8]) -> usize {
        self.stack.take_serial_input(id, output)
    }

    pub fn discard_serial_input(&mut self, id: super::ConnectionId) -> usize {
        self.stack.discard_serial_input(id)
    }

    pub fn write_serial_output(&mut self, id: super::ConnectionId, bytes: &[u8]) -> usize {
        self.stack.write_serial_output(id, bytes)
    }

    pub fn serial_output_capacity(&self, id: super::ConnectionId) -> usize {
        self.stack.serial_output_capacity(id)
    }

    pub fn ipv4_config(&self) -> Option<super::stack::Ipv4Config> {
        self.stack.ipv4_config()
    }

    pub fn dropped_transmits(&self) -> u64 {
        self.dropped_transmits
    }

    /// Gives a freshly generated management response a chance to reach the
    /// virtqueue before the main loop enters a stopped-state HLT. The command
    /// packet's TCP ACK may still occupy the sole hardware TX descriptor, so
    /// poll for a bounded period until any queued response has been promoted
    /// to that descriptor.
    pub fn flush_management_response(&mut self) -> Result<(), &'static str> {
        const MAX_POLLS: usize = 256;

        for _ in 0..MAX_POLLS {
            self.poll()?;
            if self.pending_transmit_len == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    fn transmit(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        self.reap_transmit()?;
        if self.transmit_in_flight {
            return Err("virtio-net transmit queue is busy");
        }
        let wire_len = frame.len().max(MIN_ETHERNET_FRAME);
        if VIRTIO_NET_HEADER_LEN + wire_len > BUFFER_SIZE {
            return Err("network response exceeds the transmit buffer");
        }

        let buffer =
            unsafe { slice::from_raw_parts_mut(self.transmit_buffer as *mut u8, BUFFER_SIZE) };
        buffer[..VIRTIO_NET_HEADER_LEN].fill(0);
        buffer[VIRTIO_NET_HEADER_LEN..VIRTIO_NET_HEADER_LEN + frame.len()].copy_from_slice(frame);
        buffer[VIRTIO_NET_HEADER_LEN + frame.len()..VIRTIO_NET_HEADER_LEN + wire_len].fill(0);

        self.transmit.set_descriptor(
            0,
            self.transmit_buffer,
            (VIRTIO_NET_HEADER_LEN + wire_len) as u32,
            0,
        );
        self.transmit.make_available(0);
        fence(Ordering::SeqCst);
        self.transmit_in_flight = true;
        self.transmit.notify(self.io_base);
        Ok(())
    }

    fn transmit_or_queue(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        self.reap_transmit()?;
        if !self.transmit_in_flight {
            return self.transmit(frame);
        }
        if self.pending_transmit_len != 0 {
            return Err("virtio-net pending transmit slot is busy");
        }
        if frame.len() > self.pending_transmit.len() {
            return Err("network response exceeds the pending transmit slot");
        }
        self.pending_transmit[..frame.len()].copy_from_slice(frame);
        self.pending_transmit_len = frame.len();
        Ok(())
    }

    fn flush_pending_transmit(&mut self) -> Result<(), &'static str> {
        if self.pending_transmit_len == 0 || self.transmit_in_flight {
            return Ok(());
        }
        let length = self.pending_transmit_len;
        let mut frame = [0u8; MAX_ETHERNET_FRAME];
        frame[..length].copy_from_slice(&self.pending_transmit[..length]);
        self.pending_transmit_len = 0;
        self.transmit(&frame[..length])
    }

    fn reap_transmit(&mut self) -> Result<(), &'static str> {
        while let Some(used) = self.transmit.pop_used() {
            if !self.transmit_in_flight || used.id != 0 {
                return Err("virtio-net returned an invalid transmit descriptor");
            }
            self.transmit_in_flight = false;
        }
        Ok(())
    }
}

impl Drop for VirtioNet {
    fn drop(&mut self) {
        io_write_u8(self.io_base + DEVICE_STATUS, 0);
    }
}

fn generate_management_token() -> Option<super::management::AuthToken> {
    if cpuid!(1, 0).ecx & (1 << 30) == 0 {
        return None;
    }
    let mut entropy = [0u64; 2];
    for word in &mut entropy {
        let mut generated = false;
        for _ in 0..16 {
            if unsafe { _rdrand64_step(word) } == 1 {
                generated = true;
                break;
            }
        }
        if !generated {
            return None;
        }
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = [0u8; 32];
    for (index, byte) in entropy.into_iter().flat_map(u64::to_le_bytes).enumerate() {
        token[index * 2] = HEX[(byte >> 4) as usize];
        token[index * 2 + 1] = HEX[(byte & 0x0f) as usize];
    }
    Some(token)
}

const fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

fn fail_device(io_base: u16) {
    let status = io_read_u8(io_base + DEVICE_STATUS);
    io_write_u8(io_base + DEVICE_STATUS, status | STATUS_FAILED);
}

fn io_read_u8(port: u16) -> u8 {
    unsafe { Port::<u8>::new(port).read() }
}

fn io_read_u16(port: u16) -> u16 {
    unsafe { Port::<u16>::new(port).read() }
}

fn io_read_u32(port: u16) -> u32 {
    unsafe { Port::<u32>::new(port).read() }
}

fn io_write_u8(port: u16, value: u8) {
    unsafe { Port::<u8>::new(port).write(value) }
}

fn io_write_u16(port: u16, value: u16) {
    unsafe { Port::<u16>::new(port).write(value) }
}

fn io_write_u32(port: u16, value: u32) {
    unsafe { Port::<u32>::new(port).write(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ring_layout_is_page_aligned() {
        let size = 256usize;
        let available_offset = core::mem::size_of::<Descriptor>() * size;
        let used_offset = align_up(available_offset + 6 + 2 * size, BUFFER_SIZE);
        assert_eq!(available_offset, 4096);
        assert_eq!(used_offset, 8192);
        assert_eq!(
            (used_offset + 6 + core::mem::size_of::<UsedElement>() * size).div_ceil(BUFFER_SIZE),
            3
        );
    }
}
