use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::{
    interrupt::{
        apic::{EOI, LAPIC},
        gdt,
        subscriber::InterruptContext,
    },
    time, warn,
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
    let context = InterruptContext {
        vector: 3,
        instruction_pointer: stack_frame.instruction_pointer.as_u64(),
        code_segment: stack_frame.code_segment.0 as u64,
        cpu_flags: stack_frame.cpu_flags.bits(),
        stack_pointer: stack_frame.stack_pointer.as_u64(),
        stack_segment: stack_frame.stack_segment.0 as u64,
    };

    crate::interrupt::subscriber::dispatch_to_subscribers(&context);

    warn!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    let context = InterruptContext {
        vector: 8,
        instruction_pointer: stack_frame.instruction_pointer.as_u64(),
        code_segment: stack_frame.code_segment.0 as u64,
        cpu_flags: stack_frame.cpu_flags.bits(),
        stack_pointer: stack_frame.stack_pointer.as_u64(),
        stack_segment: stack_frame.stack_segment.0 as u64,
    };

    crate::interrupt::subscriber::dispatch_to_subscribers(&context);

    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    let context = InterruptContext {
        vector: 14,
        instruction_pointer: stack_frame.instruction_pointer.as_u64(),
        code_segment: stack_frame.code_segment.0 as u64,
        cpu_flags: stack_frame.cpu_flags.bits(),
        stack_pointer: stack_frame.stack_pointer.as_u64(),
        stack_segment: stack_frame.stack_segment.0 as u64,
    };

    crate::interrupt::subscriber::dispatch_to_subscribers(&context);

    panic!(
        "EXCEPTION: PAGE FAULT\n{:#?}\nAccessed address: {:#x}",
        stack_frame,
        Cr2::read().unwrap().as_u64()
    );
}

extern "x86-interrupt" fn timer_handler(stack_frame: InterruptStackFrame) {
    let context = InterruptContext {
        vector: IRQ_TIMER as u8,
        instruction_pointer: stack_frame.instruction_pointer.as_u64(),
        code_segment: stack_frame.code_segment.0 as u64,
        cpu_flags: stack_frame.cpu_flags.bits(),
        stack_pointer: stack_frame.stack_pointer.as_u64(),
        stack_segment: stack_frame.stack_segment.0 as u64,
    };

    crate::interrupt::subscriber::dispatch_to_subscribers(&context);

    time::tick();
    LAPIC.get().unwrap().write(EOI, 0);
}
