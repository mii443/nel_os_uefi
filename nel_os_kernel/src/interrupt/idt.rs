use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::{
    info_w_int,
    interrupt::{
        apic::{EOI, LAPIC},
        gdt,
    },
    warn,
};

const PIC_8259_IRQ_OFFSET: u32 = 32;
pub const IRQ_TIMER: u32 = PIC_8259_IRQ_OFFSET + 16;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint
            .set_handler_fn(breakpoint_handler)
            .disable_interrupts(true);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX)
                .disable_interrupts(true);
        }
        idt.page_fault
            .set_handler_fn(page_fault_handler)
            .disable_interrupts(true);
        idt[IRQ_TIMER as u8]
            .set_handler_fn(timer_handler)
            .disable_interrupts(true);

        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    warn!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    panic!(
        "EXCEPTION: PAGE FAULT\n{:#?}\nAccessed address: {:#x}",
        stack_frame,
        Cr2::read().unwrap().as_u64()
    );
}

extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    LAPIC.get().unwrap().write(EOI, 0);
    info_w_int!("Timer interrupt received");
}
