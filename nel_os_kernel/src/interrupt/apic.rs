use acpi::PlatformInfo;
use alloc::alloc::Global;
use spin::Once;
use x86_64::instructions::port::Port;

use crate::{info, interrupt::idt::IRQ_TIMER};

pub static LAPIC: Once<LocalApic> = Once::new();
pub static GUEST_TSC_KHZ: Once<u64> = Once::new();

pub fn disable_pic_8259() {
    unsafe {
        Port::new(0xa1).write(0xffu8);
        Port::new(0x21).write(0xffu8);
    }
}

pub struct LocalApic {
    pub ptr: *mut u32,
}

unsafe impl Send for LocalApic {}
unsafe impl Sync for LocalApic {}

impl LocalApic {
    fn new(base: u64) -> Self {
        LocalApic {
            ptr: base as *mut u32,
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        unsafe { self.ptr.add(offset as usize).read_volatile() }
    }

    pub fn write(&self, offset: u32, value: u32) {
        unsafe { self.ptr.add(offset as usize).write_volatile(value) }
    }
}

const SVR: u32 = 0x00f0 / 4;
const ENABLE: u32 = 0x100;

const TDCR: u32 = 0x03e0 / 4;
const TCCR: u32 = 0x0390 / 4;
const TICR: u32 = 0x0380 / 4;

const TIMER: u32 = 0x0320 / 4;
const X1: u32 = 0b1011;
const PERIODIC: u32 = 0x20000;

const MASKED: u32 = 0x10000;

const ICRLO: u32 = 0x0300 / 4;
const BCAST: u32 = 0x80000;
const INIT: u32 = 0x500;
const LEVEL: u32 = 0x8000;
const DELIVS: u32 = 0x1000;

const ICRHI: u32 = 0x0310 / 4;

const PCINT: u32 = 0x0340 / 4;
const LINT0: u32 = 0x0350 / 4;
const LINT1: u32 = 0x0360 / 4;

pub const EOI: u32 = 0x00b0 / 4;

const TPR: u32 = 0x0080 / 4;

const PM_TIMER_FREQ: usize = 3579545;
const CALIBRATION_MS: usize = 100;

fn read_tsc_ordered() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!(
            "lfence",
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags),
        );
    }
    ((high as u64) << 32) | low as u64
}

pub fn init_local_apic(platform_info: PlatformInfo<'_, Global>) {
    disable_pic_8259();

    let apic_info = match platform_info.interrupt_model {
        acpi::InterruptModel::Apic(ref apic) => apic,
        _ => panic!("APIC not found in ACPI tables"),
    };

    let local_apic_base = apic_info.local_apic_address;
    let local_apic = LocalApic::new(local_apic_base);
    LAPIC.call_once(|| LocalApic::new(local_apic_base));

    local_apic.write(SVR, ENABLE | 0xff);
    local_apic.write(TDCR, X1);
    local_apic.write(TIMER, MASKED);

    // calibrate timer
    local_apic.write(TICR, u32::MAX);
    let pm_timer = platform_info
        .pm_timer
        .expect("PM Timer not found in ACPI tables");
    let mut time = Port::<u32>::new(pm_timer.base.address as u16);
    let start = unsafe { time.read() };
    let tsc_start = read_tsc_ordered();
    let calibration_ticks = PM_TIMER_FREQ * CALIBRATION_MS / 1000;
    let mut end = start.wrapping_add(calibration_ticks as u32);
    if !pm_timer.supports_32bit {
        end &= 0x00ffffff;
    }
    if end < start {
        while unsafe { time.read() } >= start {}
    }
    while unsafe { time.read() } < end {}
    let tsc_end = read_tsc_ordered();
    let local_apic_freq = u32::MAX - local_apic.read(TCCR);
    local_apic.write(TICR, 0);

    let tsc_delta = tsc_end.wrapping_sub(tsc_start);
    let tsc_khz =
        (tsc_delta as u128 * PM_TIMER_FREQ as u128 / calibration_ticks as u128 / 1000) as u64;
    if tsc_khz != 0 && u32::try_from(tsc_khz).is_ok() {
        GUEST_TSC_KHZ.call_once(|| tsc_khz);
        info!("TSC calibrated at {} kHz using ACPI PM timer", tsc_khz);
    }

    local_apic.write(TDCR, X1);
    local_apic.write(TIMER, PERIODIC | IRQ_TIMER);
    local_apic.write(TICR, local_apic_freq / 250);

    local_apic.write(LINT0, MASKED);
    local_apic.write(LINT1, MASKED);

    if (local_apic.read(0x30 / 4) >> 16) & 0xFF >= 4 {
        local_apic.write(PCINT, MASKED);
    }

    local_apic.write(EOI, 0);

    local_apic.write(ICRHI, 0);
    local_apic.write(ICRLO, BCAST | INIT | LEVEL);

    while local_apic.read(ICRLO) & DELIVS != 0 {}

    local_apic.write(TPR, 0);
}
