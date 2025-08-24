#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => ($crate::print!("[{:>12.5} I] {}\n", $crate::time::get_ticks() as f64 / 1000., format_args!($($arg)*)));
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => ($crate::print!("[{:>12.5} E] {}\n", $crate::time::get_ticks() as f64 / 1000., format_args!($($arg)*)));
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => ($crate::print!("[{:>12.5} W] {}\n", $crate::time::get_ticks() as f64 / 1000., format_args!($($arg)*)));
}
