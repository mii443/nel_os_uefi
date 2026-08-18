use alloc::boxed::Box;
use core::{
    arch::asm,
    fmt::{self, Write},
};

use crate::{
    constant::PKG_VERSION,
    cpuid, interrupt,
    memory::bitmap::BitmapMemoryTable,
    network::{Ipv4Config, ManagementCommand, VirtioNet},
    platform, serial,
    serial_console::SerialConsole,
    time, vmm, {error, info, warn},
};

#[derive(Clone, Copy)]
enum VmState {
    NotStarted,
    Running,
    Stopped,
    Failed(&'static str),
}

pub(crate) struct VmController {
    network: Option<VirtioNet>,
    serial_console: SerialConsole,
    vcpu: Option<Box<dyn vmm::VCpu>>,
    state: VmState,
    serial_owner: Option<SerialOwner>,
    network_serial_overflow_reported: bool,
    network_serial_drop_baseline: u64,
    total_frames: usize,
    boot_tsc: u64,
}

impl VmController {
    pub(crate) fn new(network: Option<VirtioNet>, total_frames: usize, boot_tsc: u64) -> Self {
        Self {
            network,
            serial_console: SerialConsole::new(),
            vcpu: None,
            state: VmState::NotStarted,
            serial_owner: None,
            network_serial_overflow_reported: false,
            network_serial_drop_baseline: serial::guest_output_drop_count(),
            total_frames,
            boot_tsc,
        }
    }

    pub(crate) fn run(mut self, allocator: &mut BitmapMemoryTable) -> ! {
        self.serial_console.activate();
        loop {
            self.poll_serial_console(allocator);
            self.poll_network(allocator);
            self.run_guest(allocator);
        }
    }

    fn poll_serial_console(&mut self, allocator: &mut BitmapMemoryTable) {
        self.serial_console.poll();
        if self.serial_owner == Some(SerialOwner::Local) && !self.serial_console.serial_attached() {
            self.serial_owner = None;
            serial::set_guest_capture_enabled(false);
        }
        while let Some(command) = self.serial_console.take_command() {
            self.handle_command(CommandSource::Serial, command, allocator);
        }
    }

    fn poll_network(&mut self, allocator: &mut BitmapMemoryTable) {
        if self.network.is_none() {
            serial::set_guest_capture_enabled(false);
            return;
        }

        if let Err(error) = self.network.as_mut().unwrap().poll() {
            error!("Hypervisor network poll failed: {}", error);
            if self.serial_owner == Some(SerialOwner::Network) {
                self.serial_owner = None;
                serial::reset_guest_bridge();
            } else {
                serial::set_guest_capture_enabled(false);
            }
            self.network = None;
            return;
        }

        // Parsing deferred TCP input can both enqueue guest serial bytes and
        // encounter Ctrl-]. Do that before the bridge drain so bytes preceding
        // a detach are delivered (or explicitly discarded below) before a
        // following lifecycle command can clear the connection buffers.
        let start_requested = self.network.as_mut().unwrap().take_start_request();
        let command = self.network.as_mut().unwrap().take_management_command();

        let mut serial_bytes = [0; 256];
        if self.serial_owner == Some(SerialOwner::Network) {
            loop {
                let guest_capacity = serial::guest_input_capacity().min(serial_bytes.len());
                if guest_capacity == 0 {
                    break;
                }
                let count = self
                    .network
                    .as_mut()
                    .unwrap()
                    .take_serial_input(&mut serial_bytes[..guest_capacity]);
                if count == 0 {
                    break;
                }
                let queued = serial::queue_guest_input(&serial_bytes[..count]);
                debug_assert_eq!(queued, count);
            }
        }

        let network_attached = self.network.as_ref().unwrap().serial_attached();
        if self.serial_owner == Some(SerialOwner::Network) && !network_attached {
            let discarded = self.network.as_mut().unwrap().discard_serial_input();
            if discarded != 0 {
                self.network
                    .as_mut()
                    .unwrap()
                    .notify_management_detach_or_close(
                        b"\r\nWARN buffered VM serial input was discarded during detach.\r\n",
                    );
            }
            self.serial_owner = None;
            self.network_serial_overflow_reported = false;
            serial::set_guest_capture_enabled(false);
        } else if self.serial_owner != Some(SerialOwner::Network) && network_attached {
            self.network.as_mut().unwrap().set_serial_attached(false);
        }

        if self.serial_owner == Some(SerialOwner::Network) {
            serial::set_guest_capture_enabled(true);
            let device = self.network.as_mut().unwrap();
            let output_capacity = device.serial_output_capacity().min(serial_bytes.len());
            let count = serial::poll_guest_output(&mut serial_bytes[..output_capacity]);
            if count != 0 {
                let written = device.write_serial_output(&serial_bytes[..count]);
                debug_assert_eq!(written, count);
            }

            let output_drops = serial::guest_output_drop_count();
            if output_drops > self.network_serial_drop_baseline
                && !self.network_serial_overflow_reported
            {
                const WARNING: &[u8] =
                    b"\r\nWARN guest serial output overflowed; bytes were dropped.\r\n";
                if device.serial_output_capacity() >= WARNING.len()
                    && device.write_serial_output(WARNING) == WARNING.len()
                {
                    self.network_serial_overflow_reported = true;
                }
            }
        } else {
            serial::set_guest_capture_enabled(false);
            self.network_serial_overflow_reported = false;
        }

        let mut management_response_pending = false;
        if start_requested {
            self.handle_command(CommandSource::Udp, ManagementCommand::VmStart, allocator);
        }
        if let Some(command) = command {
            self.handle_command(CommandSource::Network, command, allocator);
            management_response_pending = true;
        }

        // In particular, `vm stop` changes the next action from VMRUN to HLT.
        // Push its response into the virtqueue first so returning to the shell
        // does not depend on a later timer interrupt waking the stopped state.
        if management_response_pending
            && let Some(device) = self.network.as_mut()
            && let Err(error) = device.flush_management_response()
        {
            error!("Unable to flush management response: {}", error);
        }
    }

    fn handle_command(
        &mut self,
        source: CommandSource,
        command: ManagementCommand,
        allocator: &mut BitmapMemoryTable,
    ) {
        if matches!(
            command,
            ManagementCommand::VmStop | ManagementCommand::VmReset
        ) {
            self.detach_serial_owner(
                b"\r\nVM serial detached because the VM lifecycle changed.\r\n",
            );
        }

        let network_config = self.network.as_ref().and_then(VirtioNet::ipv4_config);
        let network_drops = self
            .network
            .as_ref()
            .map_or(0, VirtioNet::dropped_transmits);
        let serial_drops = serial::guest_output_drop_count();
        let source_owner = source.serial_owner();
        let endpoint_owns_serial = source_owner.is_some() && self.serial_owner == source_owner;
        let attach_allowed = source_owner.is_some()
            && (self.serial_owner.is_none() || self.serial_owner == source_owner);

        let action = match source {
            CommandSource::Network => {
                let Some(device) = self.network.as_mut() else {
                    return;
                };
                let mut endpoint = NetworkEndpoint(device);
                process_management_command(
                    command,
                    &mut endpoint,
                    &mut self.vcpu,
                    &mut self.state,
                    allocator,
                    self.total_frames,
                    self.boot_tsc,
                    network_config,
                    network_drops,
                    serial_drops,
                    attach_allowed,
                    endpoint_owns_serial,
                )
            }
            CommandSource::Serial => process_management_command(
                command,
                &mut self.serial_console,
                &mut self.vcpu,
                &mut self.state,
                allocator,
                self.total_frames,
                self.boot_tsc,
                network_config,
                network_drops,
                serial_drops,
                attach_allowed,
                endpoint_owns_serial,
            ),
            CommandSource::Udp => {
                let mut endpoint = SilentEndpoint;
                process_management_command(
                    command,
                    &mut endpoint,
                    &mut self.vcpu,
                    &mut self.state,
                    allocator,
                    self.total_frames,
                    self.boot_tsc,
                    network_config,
                    network_drops,
                    serial_drops,
                    false,
                    false,
                )
            }
        };

        match action {
            SerialAction::Attach => {
                self.serial_owner = source_owner;
                self.network_serial_overflow_reported = false;
                self.network_serial_drop_baseline = serial::guest_output_drop_count();
            }
            SerialAction::Detach if self.serial_owner == source_owner => {
                self.serial_owner = None;
                self.network_serial_overflow_reported = false;
                serial::reset_guest_bridge();
            }
            SerialAction::None | SerialAction::Detach => {}
        }
    }

    fn detach_serial_owner(&mut self, notice: &[u8]) {
        match self.serial_owner.take() {
            Some(SerialOwner::Network) => {
                if let Some(device) = self.network.as_mut() {
                    device.set_serial_attached(false);
                    device.notify_management_detach_or_close(notice);
                }
            }
            Some(SerialOwner::Local) => {
                self.serial_console.set_serial_attached(false);
                self.serial_console.write_bytes(notice);
                self.serial_console.prompt();
            }
            None => {}
        }
        self.network_serial_overflow_reported = false;
        serial::reset_guest_bridge();
    }

    fn run_guest(&mut self, allocator: &mut BitmapMemoryTable) {
        if matches!(self.state, VmState::Running) {
            let result = self
                .vcpu
                .as_mut()
                .ok_or("running VM has no VCPU")
                .and_then(|vcpu| vcpu.run(allocator));
            if let Err(error) = result {
                error!("VCPU run failed: {}", error);
                warn!("Guest stopped; keeping hypervisor management networking online");
                self.detach_serial_owner(
                    b"\r\nVM serial detached because guest execution failed.\r\n",
                );
                self.state = VmState::Failed(error);
            }
        } else {
            // The virtio-net device is deliberately polled with INTx disabled,
            // and the physical serial console is polled from this same loop,
            // so the periodic host timer bounds management polling latency.
            // Re-arm it after leaving a nested guest; some L0/L1 combinations
            // otherwise retain the LVT setup with a stopped current count.
            // Keep STI and HLT adjacent to avoid losing a wakeup between them.
            interrupt::apic::rearm_management_timer();
            unsafe {
                asm!("sti; hlt", options(nomem, nostack));
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandSource {
    Network,
    Serial,
    Udp,
}

impl CommandSource {
    fn serial_owner(self) -> Option<SerialOwner> {
        match self {
            Self::Network => Some(SerialOwner::Network),
            Self::Serial => Some(SerialOwner::Local),
            Self::Udp => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SerialOwner {
    Network,
    Local,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SerialAction {
    None,
    Attach,
    Detach,
}

trait ManagementEndpoint: Write {
    fn write_bytes(&mut self, bytes: &[u8]);
    fn set_serial_attached(&mut self, attached: bool);
    fn close_supported(&self) -> bool;
    fn request_close(&mut self);

    fn prompt(&mut self) {
        self.write_bytes(crate::management::PROMPT);
    }

    fn write_help(&mut self) {
        self.write_bytes(crate::management::HELP);
    }
}

struct NetworkEndpoint<'a>(&'a mut VirtioNet);

struct SilentEndpoint;

impl fmt::Write for SilentEndpoint {
    fn write_str(&mut self, _text: &str) -> fmt::Result {
        Ok(())
    }
}

impl ManagementEndpoint for SilentEndpoint {
    fn write_bytes(&mut self, _bytes: &[u8]) {}

    fn set_serial_attached(&mut self, _attached: bool) {}

    fn close_supported(&self) -> bool {
        false
    }

    fn request_close(&mut self) {}
}

impl fmt::Write for NetworkEndpoint<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.0.write_management(text.as_bytes()) == text.len() {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

impl ManagementEndpoint for NetworkEndpoint<'_> {
    fn write_bytes(&mut self, bytes: &[u8]) {
        self.0.write_management(bytes);
    }

    fn set_serial_attached(&mut self, attached: bool) {
        self.0.set_serial_attached(attached);
        serial::set_guest_capture_enabled(attached && self.0.serial_attached());
    }

    fn close_supported(&self) -> bool {
        true
    }

    fn request_close(&mut self) {
        self.0.request_management_close();
    }
}

impl ManagementEndpoint for SerialConsole {
    fn write_bytes(&mut self, bytes: &[u8]) {
        SerialConsole::write_bytes(self, bytes);
    }

    fn set_serial_attached(&mut self, attached: bool) {
        SerialConsole::set_serial_attached(self, attached);
    }

    fn close_supported(&self) -> bool {
        false
    }

    fn request_close(&mut self) {}

    fn prompt(&mut self) {
        SerialConsole::prompt(self);
    }

    fn write_help(&mut self) {
        SerialConsole::write_help(self);
    }
}

fn write_vm_status<E: ManagementEndpoint>(
    endpoint: &mut E,
    state: VmState,
    guest_memory_size: Option<u64>,
) {
    let state_text = match state {
        VmState::NotStarted => "not-started",
        VmState::Running => "running",
        VmState::Stopped => "stopped",
        VmState::Failed(_) => "failed",
    };
    let _ = write!(endpoint, "VM state: {}\r\n", state_text);
    if let Some(size) = guest_memory_size {
        let _ = write!(endpoint, "Guest memory: {} MiB\r\n", size / 1024 / 1024);
    }
    if let VmState::Failed(error) = state {
        let _ = write!(endpoint, "Last error: {}\r\n", error);
    }
}

fn write_memory_info<E: ManagementEndpoint>(
    endpoint: &mut E,
    allocator: &BitmapMemoryTable,
    total_frames: usize,
) {
    let free_frames = allocator.free_frame_count();
    let used_frames = total_frames.saturating_sub(free_frames);
    let _ = write!(
        endpoint,
        "Host memory: total={} MiB used={} MiB free={} MiB\r\n",
        total_frames * 4 / 1024,
        used_frames * 4 / 1024,
        free_frames * 4 / 1024
    );
}

fn write_runtime_info<E: ManagementEndpoint>(
    endpoint: &mut E,
    boot_tsc: u64,
    config: Option<Ipv4Config>,
    dropped_transmits: u64,
    serial_output_drops: u64,
) {
    let vendor = cpuid::get_vendor_id();
    let brand = cpuid::get_brand();
    let virtualization = if platform::is_amd() {
        "AMD SVM"
    } else if platform::is_intel() {
        "Intel VMX"
    } else {
        "unsupported"
    };
    let _ = write!(endpoint, "nel_os_kernel {}\r\n", PKG_VERSION);
    let _ = write!(endpoint, "CPU: {} {}\r\n", vendor, brand);
    let _ = write!(endpoint, "Virtualization: {}\r\n", virtualization);
    let _ = write!(
        endpoint,
        "Hypervisor network TX drops: {}\r\n",
        dropped_transmits
    );
    let _ = write!(
        endpoint,
        "Guest serial output drops: {}\r\n",
        serial_output_drops
    );
    if let Some(tsc_khz) = interrupt::apic::GUEST_TSC_KHZ.get() {
        let current_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let uptime_ms = current_tsc.wrapping_sub(boot_tsc) / *tsc_khz;
        let _ = write!(endpoint, "Uptime: {} ms\r\n", uptime_ms);
        let _ = write!(endpoint, "Host TSC: {} kHz\r\n", tsc_khz);
    } else {
        let _ = write!(endpoint, "Scheduler ticks: {}\r\n", time::get_ticks());
    }
    if let Some(config) = config {
        let address = config.address;
        let mask = config.subnet_mask;
        let router = config.router;
        let _ = write!(
            endpoint,
            "IPv4: {}.{}.{}.{}/{}.{}.{}.{} router {}.{}.{}.{} DHCP lease {} s\r\n",
            address[0],
            address[1],
            address[2],
            address[3],
            mask[0],
            mask[1],
            mask[2],
            mask[3],
            router[0],
            router[1],
            router[2],
            router[3],
            config.lease_seconds
        );
    } else {
        let _ = endpoint.write_str("IPv4: waiting for DHCP\r\n");
    }
}

fn process_management_command<E: ManagementEndpoint>(
    command: ManagementCommand,
    endpoint: &mut E,
    vcpu: &mut Option<Box<dyn vmm::VCpu>>,
    state: &mut VmState,
    allocator: &mut BitmapMemoryTable,
    total_frames: usize,
    boot_tsc: u64,
    network_config: Option<Ipv4Config>,
    network_drops: u64,
    serial_drops: u64,
    attach_allowed: bool,
    endpoint_owns_serial: bool,
) -> SerialAction {
    let mut add_prompt = true;
    let mut serial_action = SerialAction::None;
    match command {
        ManagementCommand::VmStart | ManagementCommand::VmStartAttach => {
            let attach = matches!(command, ManagementCommand::VmStartAttach);
            match *state {
                VmState::Running => {
                    endpoint.write_bytes(b"VM is already running.\r\n");
                }
                VmState::Failed(_) => {
                    endpoint.write_bytes(b"ERR VM failed; use 'vm reset' to recover.\r\n");
                }
                VmState::Stopped => {
                    serial::reset_guest_bridge();
                    *state = VmState::Running;
                    endpoint.write_bytes(b"VM resumed.\r\n");
                    info!("VM resumed by management shell");
                }
                VmState::NotStarted => match vmm::get_vcpu(allocator) {
                    Ok(new_vcpu) => {
                        serial::reset_guest_bridge();
                        *vcpu = Some(new_vcpu);
                        *state = VmState::Running;
                        endpoint.write_bytes(b"VM started.\r\n");
                        info!("VM started by management shell");
                    }
                    Err(error) => {
                        *state = VmState::Failed(error);
                        let _ = write!(endpoint, "ERR unable to create VM: {}\r\n", error);
                    }
                },
            }

            if attach && matches!(*state, VmState::Running) {
                if attach_allowed {
                    endpoint.write_bytes(
                        b"Attached to VM serial. Press Ctrl-] to return to the management shell.\r\n",
                    );
                    endpoint.set_serial_attached(true);
                    serial_action = SerialAction::Attach;
                    add_prompt = false;
                } else {
                    endpoint.write_bytes(b"ERR VM serial is attached to another console.\r\n");
                }
            }
        }
        ManagementCommand::VmStop => {
            if matches!(*state, VmState::Running) {
                *state = VmState::Stopped;
                endpoint.write_bytes(b"VM stopped; guest memory is retained.\r\n");
                info!("VM stopped by management shell");
            } else {
                endpoint.write_bytes(b"VM is not running.\r\n");
            }
        }
        ManagementCommand::VmReset => {
            let result = if let Some(vcpu) = vcpu.as_mut() {
                vcpu.reset()
            } else if matches!(*state, VmState::Failed(_)) {
                Err("VM construction previously failed; reboot the hypervisor to retry")
            } else {
                match vmm::get_vcpu(allocator) {
                    Ok(new_vcpu) => {
                        *vcpu = Some(new_vcpu);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            };
            match result {
                Ok(()) => {
                    serial::reset_guest_bridge();
                    *state = VmState::Running;
                    endpoint.write_bytes(b"VM reset and started.\r\n");
                    info!("VM reset by management shell");
                }
                Err(error) => {
                    *state = VmState::Failed(error);
                    let _ = write!(endpoint, "ERR unable to reset VM: {}\r\n", error);
                }
            }
        }
        ManagementCommand::VmStatus => {
            write_vm_status(
                endpoint,
                *state,
                vcpu.as_ref().map(|vcpu| vcpu.get_guest_memory_size()),
            );
        }
        ManagementCommand::SerialAttach => {
            if !matches!(*state, VmState::Running) {
                endpoint.write_bytes(b"ERR VM serial is available only while running.\r\n");
            } else if !attach_allowed {
                endpoint.write_bytes(b"ERR VM serial is attached to another console.\r\n");
            } else {
                endpoint.write_bytes(
                    b"Attached to VM serial. Press Ctrl-] to return to the management shell.\r\n",
                );
                endpoint.set_serial_attached(true);
                serial_action = SerialAction::Attach;
                add_prompt = false;
            }
        }
        ManagementCommand::SerialDetach => {
            if endpoint_owns_serial {
                endpoint.set_serial_attached(false);
                endpoint.write_bytes(b"VM serial is detached.\r\n");
                serial_action = SerialAction::Detach;
            } else {
                endpoint.write_bytes(b"VM serial is already detached from this console.\r\n");
            }
        }
        ManagementCommand::InfoMemory => write_memory_info(endpoint, allocator, total_frames),
        ManagementCommand::InfoRuntime => write_runtime_info(
            endpoint,
            boot_tsc,
            network_config,
            network_drops,
            serial_drops,
        ),
        ManagementCommand::InfoAll => {
            write_runtime_info(
                endpoint,
                boot_tsc,
                network_config,
                network_drops,
                serial_drops,
            );
            write_memory_info(endpoint, allocator, total_frames);
            write_vm_status(
                endpoint,
                *state,
                vcpu.as_ref().map(|vcpu| vcpu.get_guest_memory_size()),
            );
        }
        ManagementCommand::Help => endpoint.write_help(),
        ManagementCommand::Prompt => {
            endpoint.prompt();
            add_prompt = false;
        }
        ManagementCommand::Invalid => {
            endpoint.write_bytes(b"ERR unknown command; type 'help'\r\n");
        }
        ManagementCommand::Disconnect => {
            if endpoint.close_supported() {
                endpoint.write_bytes(b"Bye.\r\n");
                endpoint.request_close();
                add_prompt = false;
            } else {
                endpoint.write_bytes(b"The local management console remains active.\r\n");
            }
        }
    }
    if add_prompt {
        endpoint.prompt();
    }
    serial_action
}
