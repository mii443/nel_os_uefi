use core::sync::atomic::AtomicBool;

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
}

static OUTPUT_TO_SCREEN: AtomicBool = AtomicBool::new(true);

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
