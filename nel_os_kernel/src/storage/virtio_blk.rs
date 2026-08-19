use core::{
    ptr::{read_volatile, write_volatile},
    slice,
    sync::atomic::{Ordering, fence},
};

use x86_64::{
    PhysAddr,
    instructions::port::Port,
    structures::paging::{FrameAllocator, PageSize, PhysFrame, Size4KiB},
};

use crate::{
    info,
    network::pci::{self, PciAddress},
    time,
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

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

const REQUEST_QUEUE: u16 = 0;
const REQUEST_HEADER_OFFSET: u64 = 0;
const REQUEST_DATA_OFFSET: u64 = 512;
const REQUEST_STATUS_OFFSET: u64 = 1024;
const SECTOR_SIZE: usize = 512;
const BUFFER_SIZE: usize = Size4KiB::SIZE as usize;
const REQUEST_TIMEOUT_MILLIS: usize = 1_000;
const REQUEST_SPIN_LIMIT: usize = 10_000_000;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

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

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RequestHeader {
    request_type: u32,
    reserved: u32,
    sector: u64,
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
        allocator: &mut dyn FrameAllocator<Size4KiB>,
    ) -> Result<Self, &'static str> {
        io_write_u16(io_base + QUEUE_SELECT, index);
        let size = io_read_u16(io_base + QUEUE_SIZE);
        if size < 3 || !size.is_power_of_two() {
            return Err("virtio-blk exposed an invalid request queue size");
        }

        let available_offset = core::mem::size_of::<Descriptor>() * size as usize;
        let used_offset = align_up(available_offset + 6 + 2 * size as usize, BUFFER_SIZE);
        let bytes = used_offset + 6 + core::mem::size_of::<UsedElement>() * size as usize;
        let pages = bytes.div_ceil(BUFFER_SIZE);
        let memory = allocate_contiguous(allocator, pages)?;
        let physical = memory.start_address().as_u64();
        if physical >> 12 > u32::MAX as u64 {
            return Err("virtio-blk queue memory is above its 44-bit DMA limit");
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

    fn set_descriptor(&mut self, id: u16, address: u64, length: u32, flags: u16, next: u16) {
        debug_assert!(id < self.size);
        unsafe {
            write_volatile(
                self.descriptor.add(id as usize),
                Descriptor {
                    address,
                    length,
                    flags,
                    next,
                },
            );
        }
    }

    fn make_available(&mut self, id: u16) {
        let available_index = unsafe { read_volatile(self.available.add(2) as *const u16) };
        let slot = available_index % self.size;
        unsafe {
            write_volatile(self.available.add(4 + slot as usize * 2) as *mut u16, id);
        }
        fence(Ordering::Release);
        unsafe {
            write_volatile(
                self.available.add(2) as *mut u16,
                available_index.wrapping_add(1),
            );
        }
    }

    fn pop_used(&mut self) -> Option<UsedElement> {
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
        Some(element)
    }

    fn write_available_flags(&self, value: u16) {
        unsafe { write_volatile(self.available as *mut u16, value) }
    }

    fn notify(&self, io_base: u16) {
        io_write_u16(io_base + QUEUE_NOTIFY, self.index);
    }
}

// Queue pointers refer to reserved identity-mapped physical pages. The outer
// kernel polls the queue only from its bootstrap CPU.
unsafe impl Send for VirtQueue {}

pub struct VirtioBlock {
    pci_address: PciAddress,
    io_base: u16,
    queue: VirtQueue,
    _request_memory: PhysFrame<Size4KiB>,
    request_address: u64,
    capacity_sectors: u64,
}

impl VirtioBlock {
    pub fn probe(allocator: &mut dyn FrameAllocator<Size4KiB>) -> Result<Self, &'static str> {
        let pci_address = pci::find_legacy_virtio_block()
            .ok_or("no transitional virtio-blk PCI device was found")?;
        let io_base = pci::io_bar(pci_address).ok_or("virtio-blk has no legacy I/O BAR")?;
        pci::enable_io_bus_mastering(pci_address);

        io_write_u8(io_base + DEVICE_STATUS, 0);
        io_write_u8(io_base + DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        // The baseline request format needs no optional features. In
        // particular, every exposed sector is the architectural 512 bytes.
        let _device_features = io_read_u32(io_base + DEVICE_FEATURES);
        io_write_u32(io_base + GUEST_FEATURES, 0);

        let queue = VirtQueue::create(io_base, REQUEST_QUEUE, allocator).inspect_err(|_| {
            fail_device(io_base);
        })?;
        let Some(request_memory) = allocator.allocate_frame() else {
            fail_device(io_base);
            return Err("no DMA frame for virtio-blk requests");
        };
        let request_address = request_memory.start_address().as_u64();
        unsafe {
            core::ptr::write_bytes(request_address as *mut u8, 0, BUFFER_SIZE);
        }

        let capacity_sectors = read_config_u64(io_base + DEVICE_CONFIG);
        if capacity_sectors == 0 {
            fail_device(io_base);
            return Err("virtio-blk reports zero capacity");
        }

        io_write_u8(
            io_base + DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK,
        );
        if io_read_u8(io_base + DEVICE_STATUS) & STATUS_FAILED != 0 {
            return Err("virtio-blk rejected driver initialization");
        }

        let mut device = Self {
            pci_address,
            io_base,
            queue,
            _request_memory: request_memory,
            request_address,
            capacity_sectors,
        };
        let mut first_sector = [0u8; SECTOR_SIZE];
        device.read_sector(0, &mut first_sector)?;
        info!(
            "Host virtio-blk at {:02x}:{:02x}.{} I/O {:#x}, {} sectors ({} MiB), sector 0 readable",
            device.pci_address.bus,
            device.pci_address.device,
            device.pci_address.function,
            device.io_base,
            device.capacity_sectors,
            device.capacity_bytes() / 1024 / 1024,
        );
        Ok(device)
    }

    pub fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.capacity_sectors.saturating_mul(SECTOR_SIZE as u64)
    }

    pub fn read_sector(
        &mut self,
        sector: u64,
        output: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), &'static str> {
        self.submit(
            VIRTIO_BLK_T_IN,
            sector,
            self.request_address + REQUEST_DATA_OFFSET,
            SECTOR_SIZE as u32,
        )?;
        let data = unsafe {
            slice::from_raw_parts(
                (self.request_address + REQUEST_DATA_OFFSET) as *const u8,
                SECTOR_SIZE,
            )
        };
        output.copy_from_slice(data);
        Ok(())
    }

    pub fn write_sector(
        &mut self,
        sector: u64,
        input: &[u8; SECTOR_SIZE],
    ) -> Result<(), &'static str> {
        if sector >= self.capacity_sectors {
            return Err("virtio-blk write is beyond the device capacity");
        }
        let data = unsafe {
            slice::from_raw_parts_mut(
                (self.request_address + REQUEST_DATA_OFFSET) as *mut u8,
                SECTOR_SIZE,
            )
        };
        data.copy_from_slice(input);
        self.submit(
            VIRTIO_BLK_T_OUT,
            sector,
            self.request_address + REQUEST_DATA_OFFSET,
            SECTOR_SIZE as u32,
        )
    }

    fn submit(
        &mut self,
        request_type: u32,
        sector: u64,
        data_address: u64,
        data_length: u32,
    ) -> Result<(), &'static str> {
        if data_length == 0 || data_length as usize % SECTOR_SIZE != 0 {
            return Err("virtio-blk transfer is not sector aligned");
        }
        let sectors = u64::from(data_length) / SECTOR_SIZE as u64;
        if sector
            .checked_add(sectors)
            .is_none_or(|end| end > self.capacity_sectors)
        {
            return Err("virtio-blk request is beyond the device capacity");
        }

        unsafe {
            write_volatile(
                (self.request_address + REQUEST_HEADER_OFFSET) as *mut RequestHeader,
                RequestHeader {
                    request_type,
                    reserved: 0,
                    sector,
                },
            );
            write_volatile(
                (self.request_address + REQUEST_STATUS_OFFSET) as *mut u8,
                u8::MAX,
            );
        }

        self.queue.set_descriptor(
            0,
            self.request_address + REQUEST_HEADER_OFFSET,
            core::mem::size_of::<RequestHeader>() as u32,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        self.queue.set_descriptor(
            1,
            data_address,
            data_length,
            VIRTQ_DESC_F_NEXT
                | if request_type == VIRTIO_BLK_T_IN {
                    VIRTQ_DESC_F_WRITE
                } else {
                    0
                },
            2,
        );
        self.queue.set_descriptor(
            2,
            self.request_address + REQUEST_STATUS_OFFSET,
            1,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        fence(Ordering::SeqCst);
        self.queue.make_available(0);
        fence(Ordering::SeqCst);
        self.queue.notify(self.io_base);

        let start = time::get_ticks();
        for _ in 0..REQUEST_SPIN_LIMIT {
            if let Some(used) = self.queue.pop_used() {
                let expected_length = if request_type == VIRTIO_BLK_T_IN {
                    data_length.saturating_add(1)
                } else {
                    1
                };
                if used.id != 0 || used.length != expected_length {
                    return Err("virtio-blk returned an invalid used descriptor");
                }
                let _ = io_read_u8(self.io_base + ISR_STATUS);
                let status = unsafe {
                    read_volatile((self.request_address + REQUEST_STATUS_OFFSET) as *const u8)
                };
                return match status {
                    VIRTIO_BLK_S_OK => Ok(()),
                    VIRTIO_BLK_S_IOERR => Err("virtio-blk reported an I/O error"),
                    VIRTIO_BLK_S_UNSUPP => Err("virtio-blk does not support the request"),
                    _ => Err("virtio-blk returned an invalid request status"),
                };
            }
            if time::get_ticks().wrapping_sub(start) >= REQUEST_TIMEOUT_MILLIS {
                return Err("virtio-blk request timed out");
            }
            core::hint::spin_loop();
        }
        Err("virtio-blk request exceeded the polling limit")
    }
}

impl Drop for VirtioBlock {
    fn drop(&mut self) {
        io_write_u8(self.io_base + DEVICE_STATUS, 0);
    }
}

fn allocate_contiguous(
    allocator: &mut dyn FrameAllocator<Size4KiB>,
    pages: usize,
) -> Result<PhysFrame<Size4KiB>, &'static str> {
    let mut run_start = None;
    let mut run_length = 0usize;
    while let Some(frame) = allocator.allocate_frame() {
        let address = frame.start_address().as_u64();
        match run_start {
            Some(start) if address == start + run_length as u64 * Size4KiB::SIZE => {
                run_length += 1;
            }
            _ => {
                run_start = Some(address);
                run_length = 1;
            }
        }
        if run_length == pages {
            return PhysFrame::from_start_address(PhysAddr::new(run_start.unwrap()))
                .map_err(|_| "virtio-blk DMA allocation was not page aligned");
        }
    }
    Err("no contiguous DMA memory for a virtio-blk queue")
}

const fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

fn fail_device(io_base: u16) {
    let status = io_read_u8(io_base + DEVICE_STATUS);
    io_write_u8(io_base + DEVICE_STATUS, status | STATUS_FAILED);
}

fn read_config_u64(port: u16) -> u64 {
    u64::from(io_read_u32(port)) | (u64::from(io_read_u32(port + 4)) << 32)
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
    fn legacy_request_queue_layout_is_page_aligned() {
        let size = 128usize;
        let available_offset = core::mem::size_of::<Descriptor>() * size;
        let used_offset = align_up(available_offset + 6 + 2 * size, BUFFER_SIZE);
        assert_eq!(available_offset, 2048);
        assert_eq!(used_offset, 4096);
        assert_eq!(
            (used_offset + 6 + core::mem::size_of::<UsedElement>() * size).div_ceil(BUFFER_SIZE),
            2
        );
    }
}
