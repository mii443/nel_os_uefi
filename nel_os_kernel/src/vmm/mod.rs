pub trait VCpu {
    fn init() -> Self;
    fn run(&mut self);
}
