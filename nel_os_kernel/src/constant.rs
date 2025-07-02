pub const BANNER: &str = r#"            _              
 _ __   ___| |    ___  ___ 
| '_ \ / _ \ |   / _ \/ __|
| | | |  __/ |  | (_) \__ \
|_| |_|\___|_|___\___/|___/
            |_____|        "#;

pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");

pub const KERNEL_STACK_SIZE: usize = 1024 * 1024;
pub const HEAP_START: usize = 0x4444_4444_0000;
pub const HEAP_SIZE: usize = 128 * 1024;

pub const PAGE_SIZE: usize = 4096;
pub const MAX_MEMORY: usize = 16 * 1024 * 1024 * 1024;
pub const FRAME_COUNT: usize = MAX_MEMORY / PAGE_SIZE;
pub const BITS_PER_ENTRY: usize = 8 * core::mem::size_of::<usize>();
pub const ENTRY_COUNT: usize = FRAME_COUNT / BITS_PER_ENTRY;
