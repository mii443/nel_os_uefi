use core::arch::asm;
use core::sync::atomic::AtomicUsize;

pub static TICKS: AtomicUsize = AtomicUsize::new(0);
const HOST_TIMER_PERIOD_MILLIS: usize = 4;

#[inline(always)]
pub fn tick() {
    TICKS.fetch_add(
        HOST_TIMER_PERIOD_MILLIS,
        core::sync::atomic::Ordering::Release,
    );
}

#[inline(always)]
pub fn get_ticks() -> usize {
    TICKS.load(core::sync::atomic::Ordering::Acquire)
}

#[inline(always)]
pub fn wait_for_ms(ms: usize) {
    let start = get_ticks();
    while get_ticks() - start < ms {
        unsafe {
            asm!("nop");
        }
    }
}
