#![no_main]
#![no_std]

use log::info;
use uefi::prelude::*;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    let boot_services = uefi::env::boot_services();

    info!("Hello world!");
    boot::stall(10_000_000);
    Status::SUCCESS
}
