#![no_main]
#![no_std]

use core::arch::asm;
use uefi::{boot::MemoryType, mem::memory_map::MemoryMap, prelude::*, println};

fn hlt_loop() -> ! {
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    uefi::system::with_stdout(|stdout| stdout.clear()).unwrap();

    let memory_map = uefi::boot::memory_map(MemoryType::LOADER_DATA).unwrap();
    println!("memory_map len: {}", memory_map.len());

    println!("Conventional memory:");
    for entry in memory_map.entries() {
        if entry.ty != MemoryType::CONVENTIONAL {
            continue;
        }
        println!("    Size: {:?}MiB", entry.page_count * 4 / 1024);
        println!("    PhysStart: {:?}", entry.phys_start);
    }

    hlt_loop();
}
