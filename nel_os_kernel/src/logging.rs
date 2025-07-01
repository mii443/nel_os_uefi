#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => ($crate::print!("[info] {}\n", format_args!($($arg)*)));
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => ($crate::print!("[error] {}\n", format_args!($($arg)*)));
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => ($crate::print!("[warn] {}\n", format_args!($($arg)*)));
}
