pub const BANNER: &str = r#"            _              
 _ __   ___| |    ___  ___ 
| '_ \ / _ \ |   / _ \/ __|
| | | |  __/ |  | (_) \__ \
|_| |_|\___|_|___\___/|___/
            |_____|        "#;

pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");

pub const KERNEL_STACK_SIZE: usize = 1024 * 1024;
