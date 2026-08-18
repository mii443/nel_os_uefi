use core::sync::atomic::{AtomicBool, AtomicU64};

use alloc::format;
use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;

use crate::graphics::FRAME_BUFFER;

lazy_static! {
    pub static ref SERIAL1: Mutex<SerialPort> = {
        let mut serial_port = unsafe { SerialPort::new(0x3F8) };
        serial_port.init();
        Mutex::new(serial_port)
    };
    static ref GUEST_SERIAL_BRIDGE: Mutex<GuestSerialBridge> = Mutex::new(GuestSerialBridge::new());
}

const GUEST_SERIAL_QUEUE_LEN: usize = 4096;

struct ByteRing {
    bytes: [u8; GUEST_SERIAL_QUEUE_LEN],
    head: usize,
    len: usize,
}

impl ByteRing {
    const fn new() -> Self {
        Self {
            bytes: [0; GUEST_SERIAL_QUEUE_LEN],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, byte: u8) -> bool {
        if self.len == self.bytes.len() {
            return false;
        }
        let tail = (self.head + self.len) % self.bytes.len();
        self.bytes[tail] = byte;
        self.len += 1;
        true
    }

    fn pop_into(&mut self, output: &mut [u8]) -> usize {
        let count = output.len().min(self.len);
        for slot in &mut output[..count] {
            *slot = self.bytes[self.head];
            self.head = (self.head + 1) % self.bytes.len();
        }
        self.len -= count;
        count
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.len
    }
}

struct GuestSerialBridge {
    input: ByteRing,
    output: ByteRing,
}

impl GuestSerialBridge {
    const fn new() -> Self {
        Self {
            input: ByteRing::new(),
            output: ByteRing::new(),
        }
    }
}

static OUTPUT_TO_SCREEN: AtomicBool = AtomicBool::new(true);
static GUEST_SERIAL_CAPTURE_ENABLED: AtomicBool = AtomicBool::new(false);
static LOCAL_GUEST_OUTPUT_ENABLED: AtomicBool = AtomicBool::new(false);
static GUEST_SERIAL_BRIDGE_ACTIVE: AtomicBool = AtomicBool::new(false);
static GUEST_SERIAL_OUTPUT_DROPS: AtomicU64 = AtomicU64::new(0);

pub fn disable_screen_output() {
    OUTPUT_TO_SCREEN.store(false, core::sync::atomic::Ordering::Relaxed);
}

pub fn enable_screen_output() {
    OUTPUT_TO_SCREEN.store(true, core::sync::atomic::Ordering::Relaxed);
}

pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        SERIAL1
            .lock()
            .write_fmt(args)
            .expect("Printing to serial failed");

        if !OUTPUT_TO_SCREEN.load(core::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let mut fb = FRAME_BUFFER.lock();
        let fb = fb.as_mut();

        if let Some(frame_buffer) = fb {
            frame_buffer.print_text(format!("{args}").as_str());
        }
    });
}

#[inline(always)]
pub fn write_byte(byte: u8) {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        SERIAL1.lock().send(byte);
    });
}

/// Sends one byte without terminal-oriented backspace expansion.
///
/// This is used by the virtual UART, whose guest-visible byte stream must be
/// relayed verbatim to the fixed host COM1 device.
#[inline(always)]
pub fn write_raw_byte(byte: u8) {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        SERIAL1.lock().send_raw(byte);
    });
}

/// Routes a guest UART byte only to explicitly attached consoles.
///
/// The physical COM1 console is owned by the hypervisor management shell by
/// default. A network attachment uses the bounded mirror queue, while a local
/// attachment writes directly to COM1.
#[inline(always)]
pub fn write_guest_raw_byte(byte: u8) {
    if !GUEST_SERIAL_BRIDGE_ACTIVE.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    if LOCAL_GUEST_OUTPUT_ENABLED.load(core::sync::atomic::Ordering::Acquire) {
        write_raw_byte(byte);
    }
    if GUEST_SERIAL_CAPTURE_ENABLED.load(core::sync::atomic::Ordering::Acquire) {
        if !GUEST_SERIAL_BRIDGE.lock().output.push(byte) {
            GUEST_SERIAL_OUTPUT_DROPS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
    }
}

pub fn set_local_guest_output_enabled(enabled: bool) {
    LOCAL_GUEST_OUTPUT_ENABLED.store(enabled, core::sync::atomic::Ordering::Release);
}

pub fn set_guest_capture_enabled(enabled: bool) {
    let previous = GUEST_SERIAL_CAPTURE_ENABLED.swap(enabled, core::sync::atomic::Ordering::AcqRel);
    if previous == enabled {
        return;
    }

    let mut bridge = GUEST_SERIAL_BRIDGE.lock();
    bridge.output.clear();
}

/// Selects whether the currently executing VCPU owns the shared management
/// serial bridge. The controller enables this only for the attached VM's time
/// slice, so input and output cannot cross VM boundaries.
pub fn set_guest_bridge_active(active: bool) {
    GUEST_SERIAL_BRIDGE_ACTIVE.store(active, core::sync::atomic::Ordering::Release);
}

pub fn queue_guest_input(bytes: &[u8]) -> usize {
    let mut bridge = GUEST_SERIAL_BRIDGE.lock();
    let mut written = 0;
    for &byte in bytes {
        if !bridge.input.push(byte) {
            break;
        }
        written += 1;
    }
    written
}

pub fn guest_input_capacity() -> usize {
    GUEST_SERIAL_BRIDGE.lock().input.remaining()
}

pub fn discard_guest_input() -> usize {
    let mut bridge = GUEST_SERIAL_BRIDGE.lock();
    let discarded = bridge.input.len;
    bridge.input.clear();
    discarded
}

pub fn poll_guest_input(output: &mut [u8]) -> usize {
    if !GUEST_SERIAL_BRIDGE_ACTIVE.load(core::sync::atomic::Ordering::Acquire) {
        return 0;
    }
    GUEST_SERIAL_BRIDGE.lock().input.pop_into(output)
}

pub fn poll_guest_output(output: &mut [u8]) -> usize {
    GUEST_SERIAL_BRIDGE.lock().output.pop_into(output)
}

pub fn guest_output_drop_count() -> u64 {
    GUEST_SERIAL_OUTPUT_DROPS.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn reset_guest_bridge() {
    GUEST_SERIAL_BRIDGE_ACTIVE.store(false, core::sync::atomic::Ordering::Release);
    GUEST_SERIAL_CAPTURE_ENABLED.store(false, core::sync::atomic::Ordering::Release);
    LOCAL_GUEST_OUTPUT_ENABLED.store(false, core::sync::atomic::Ordering::Release);
    let mut bridge = GUEST_SERIAL_BRIDGE.lock();
    bridge.input.clear();
    bridge.output.clear();
}

/// Non-blockingly polls the fixed host COM1 device into a bounded buffer.
#[inline(always)]
pub fn poll_input(buffer: &mut [u8]) -> usize {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        let mut serial = SERIAL1.lock();
        let mut received = 0;
        for slot in buffer {
            let Ok(value) = serial.try_receive() else {
                break;
            };
            *slot = value;
            received += 1;
        }
        received
    })
}

#[inline(always)]
pub fn write_bytes(bytes: &[u8]) {
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        let mut serial = SERIAL1.lock();
        for &b in bytes {
            serial.send(b);
        }
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*));
    };
}

#[macro_export]
macro_rules! println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::print!(
        concat!($fmt, "\n"), $($arg)*));
}
