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
    NotCreated,
    Created,
    Running,
    Stopped,
    Failed(&'static str),
}

struct VirtualMachine {
    vcpu: Option<Box<dyn vmm::VCpu>>,
    state: VmState,
    configured_memory: u64,
    cpu_cycles: u64,
    accounting_start_tsc: u64,
}

impl VirtualMachine {
    fn new() -> Self {
        Self {
            vcpu: None,
            state: VmState::NotCreated,
            configured_memory: 0,
            cpu_cycles: 0,
            accounting_start_tsc: 0,
        }
    }

    fn state_text(&self) -> &'static str {
        match self.state {
            VmState::NotCreated => "not-created",
            VmState::Created => "created",
            VmState::Running => "running",
            VmState::Stopped => "stopped",
            VmState::Failed(_) => "failed",
        }
    }

    fn allocated_memory(&self) -> u64 {
        self.vcpu
            .as_ref()
            .map_or(0, |vcpu| vcpu.get_allocated_guest_memory_size())
    }

    fn memory_usage_tenths(&self) -> u64 {
        percentage_tenths(self.allocated_memory(), self.configured_memory)
    }

    fn cpu_usage_tenths(&self, now: u64) -> u64 {
        if self.accounting_start_tsc == 0 {
            return 0;
        }
        percentage_tenths(self.cpu_cycles, now.wrapping_sub(self.accounting_start_tsc)).min(1000)
    }
}

fn percentage_tenths(value: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    ((value as u128).saturating_mul(1000) / total as u128) as u64
}

pub(crate) struct VmController {
    network: Option<VirtioNet>,
    serial_console: SerialConsole,
    vms: [VirtualMachine; vmm::MAX_VMS],
    next_vm: usize,
    serial_owner: Option<SerialOwner>,
    serial_vm_id: usize,
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
            vms: core::array::from_fn(|_| VirtualMachine::new()),
            next_vm: 0,
            serial_owner: None,
            serial_vm_id: 0,
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
            let discarded = serial::discard_guest_input();
            if discarded != 0 {
                self.serial_console.write_bytes(
                    b"\r\nWARN buffered VM serial input was discarded during detach.\r\n",
                );
                self.serial_console.prompt();
            }
            self.serial_owner = None;
            serial::reset_guest_bridge();
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
            let connection_discarded = self.network.as_mut().unwrap().discard_serial_input();
            let bridge_discarded = serial::discard_guest_input();
            if connection_discarded != 0 || bridge_discarded != 0 {
                self.network
                    .as_mut()
                    .unwrap()
                    .notify_management_detach_or_close(
                        b"\r\nWARN buffered VM serial input was discarded during detach.\r\n",
                    );
            }
            self.serial_owner = None;
            self.network_serial_overflow_reported = false;
            serial::reset_guest_bridge();
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
            self.handle_command(
                CommandSource::Udp,
                ManagementCommand::VmStart {
                    id: crate::management::DEFAULT_VM_ID,
                    attach: false,
                },
                allocator,
            );
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
        if command.changes_vm_lifecycle() && command.vm_id() == Some(self.serial_vm_id) {
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
        let requested_vm = command.vm_id();
        let attach_allowed = source_owner.is_some()
            && (self.serial_owner.is_none()
                || (self.serial_owner == source_owner && requested_vm == Some(self.serial_vm_id)));

        let action = match source {
            CommandSource::Network => {
                let Some(device) = self.network.as_mut() else {
                    return;
                };
                let mut endpoint = NetworkEndpoint(device);
                process_management_command(
                    command,
                    &mut endpoint,
                    &mut self.vms,
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
                &mut self.vms,
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
                    &mut self.vms,
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
            SerialAction::Attach(vm_id) => {
                self.serial_owner = source_owner;
                self.serial_vm_id = vm_id;
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
        let Some(vm_id) = (0..vmm::MAX_VMS)
            .map(|offset| (self.next_vm + offset) % vmm::MAX_VMS)
            .find(|&vm_id| matches!(self.vms[vm_id].state, VmState::Running))
        else {
            serial::set_guest_bridge_active(false);
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
            return;
        };

        self.next_vm = (vm_id + 1) % vmm::MAX_VMS;
        let owns_serial = self.serial_owner.is_some() && self.serial_vm_id == vm_id;
        serial::set_guest_bridge_active(owns_serial);
        // Force a bounded VMEXIT even for a compute-bound guest, including on
        // nested-hypervisor combinations that stop the LAPIC current count.
        interrupt::apic::rearm_management_timer();
        let slice_start = unsafe { core::arch::x86_64::_rdtsc() };
        let slice_cycles = interrupt::apic::GUEST_TSC_KHZ
            .get()
            .copied()
            .unwrap_or(1)
            .saturating_mul(vmm::VCPU_TIME_SLICE_MILLIS);
        let result = match self.vms[vm_id].vcpu.as_mut() {
            Some(vcpu) => loop {
                if let Err(error) = vcpu.run(allocator) {
                    break Err(error);
                }
                let now = unsafe { core::arch::x86_64::_rdtsc() };
                if vcpu.is_idle() || now.wrapping_sub(slice_start) >= slice_cycles {
                    break Ok(());
                }
            },
            None => Err("running VM has no VCPU"),
        };
        let slice_end = unsafe { core::arch::x86_64::_rdtsc() };
        self.vms[vm_id].cpu_cycles = self.vms[vm_id]
            .cpu_cycles
            .wrapping_add(slice_end.wrapping_sub(slice_start));
        serial::set_guest_bridge_active(false);

        if let Err(error) = result {
            error!("VM {} VCPU run failed: {}", vm_id, error);
            warn!(
                "Guest {} stopped; keeping hypervisor management networking online",
                vm_id
            );
            if self.serial_owner.is_some() && self.serial_vm_id == vm_id {
                self.detach_serial_owner(
                    b"\r\nVM serial detached because guest execution failed.\r\n",
                );
            }
            self.vms[vm_id].state = VmState::Failed(error);
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
    Attach(usize),
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

fn write_vm_status<E: ManagementEndpoint>(endpoint: &mut E, vm_id: usize, vm: &VirtualMachine) {
    let allocated_mib = vm.allocated_memory() / 1024 / 1024;
    let configured_mib = vm.configured_memory / 1024 / 1024;
    let memory_usage = vm.memory_usage_tenths();
    let cpu_usage = vm.cpu_usage_tenths(unsafe { core::arch::x86_64::_rdtsc() });
    let _ = write!(endpoint, "VM {} state: {}\r\n", vm_id, vm.state_text());
    let _ = write!(endpoint, "vCPUs: {}\r\n", vmm::VCPUS_PER_VM);
    let _ = write!(
        endpoint,
        "Memory allocation: {}/{} MiB ({}.{:01}%)\r\n",
        allocated_mib,
        configured_mib,
        memory_usage / 10,
        memory_usage % 10
    );
    let _ = write!(
        endpoint,
        "CPU usage: {}.{:01}%\r\n",
        cpu_usage / 10,
        cpu_usage % 10
    );
    if let VmState::Failed(error) = vm.state {
        let _ = write!(endpoint, "Last error: {}\r\n", error);
    }
}

fn write_vm_list<E: ManagementEndpoint>(endpoint: &mut E, vms: &[VirtualMachine; vmm::MAX_VMS]) {
    let now = unsafe { core::arch::x86_64::_rdtsc() };
    for (vm_id, vm) in vms.iter().enumerate() {
        let memory_usage = vm.memory_usage_tenths();
        let cpu_usage = vm.cpu_usage_tenths(now);
        let _ = write!(
            endpoint,
            "VM {}: {}, {} vCPU, memory={}/{} MiB ({}.{:01}%), cpu={}.{:01}%\r\n",
            vm_id,
            vm.state_text(),
            vmm::VCPUS_PER_VM,
            vm.allocated_memory() / 1024 / 1024,
            vm.configured_memory / 1024 / 1024,
            memory_usage / 10,
            memory_usage % 10,
            cpu_usage / 10,
            cpu_usage % 10,
        );
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
    vms: &mut [VirtualMachine; vmm::MAX_VMS],
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
        ManagementCommand::VmList => write_vm_list(endpoint, vms),
        ManagementCommand::VmCreate { id, memory_mib } => {
            let vm_id = id as usize;
            if vm_id >= vmm::MAX_VMS {
                write_invalid_vm_id(endpoint, vm_id);
            } else if !matches!(vms[vm_id].state, VmState::NotCreated) {
                let _ = write!(
                    endpoint,
                    "ERR VM {} already exists; state is {}.\r\n",
                    vm_id,
                    vms[vm_id].state_text()
                );
            } else if !(vmm::MIN_GUEST_MEMORY_MIB..=vmm::MAX_GUEST_MEMORY_MIB).contains(&memory_mib)
            {
                let _ = write!(
                    endpoint,
                    "ERR guest memory must be {}-{} MiB.\r\n",
                    vmm::MIN_GUEST_MEMORY_MIB,
                    vmm::MAX_GUEST_MEMORY_MIB
                );
            } else {
                const HOST_RESERVE_MIB: usize = 128;
                const VCPU_OVERHEAD_MIB: usize = 8;
                let guest_frames = memory_mib as usize * 1024 / 4;
                let reserved_frames = (HOST_RESERVE_MIB + VCPU_OVERHEAD_MIB) * 1024 / 4;
                if allocator.free_frame_count() < guest_frames.saturating_add(reserved_frames) {
                    let _ = write!(
                        endpoint,
                        "ERR insufficient host memory to create VM {} with {} MiB while retaining the management reserve.\r\n",
                        vm_id, memory_mib
                    );
                } else {
                    let memory_size = memory_mib as u64 * 1024 * 1024;
                    let vm = &mut vms[vm_id];
                    vm.configured_memory = memory_size;
                    vm.accounting_start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
                    match vmm::get_vcpu(allocator, vm_id, memory_size) {
                        Ok(mut new_vcpu) => {
                            let prepare_result = new_vcpu.prepare(allocator);
                            vm.vcpu = Some(new_vcpu);
                            match prepare_result {
                                Ok(()) => {
                                    vm.state = VmState::Created;
                                    let _ = write!(
                                        endpoint,
                                        "VM {} created with {} MiB and {} vCPU; use 'vm start {}'.\r\n",
                                        vm_id,
                                        memory_mib,
                                        vmm::VCPUS_PER_VM,
                                        vm_id
                                    );
                                    info!(
                                        "VM {} created with {} MiB by management shell",
                                        vm_id, memory_mib
                                    );
                                }
                                Err(error) => {
                                    vm.state = VmState::Failed(error);
                                    let _ = write!(
                                        endpoint,
                                        "ERR unable to prepare VM {}: {}\r\n",
                                        vm_id, error
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            vm.state = VmState::Failed(error);
                            let _ = write!(
                                endpoint,
                                "ERR unable to create VM {}: {}\r\n",
                                vm_id, error
                            );
                        }
                    }
                }
            }
        }
        ManagementCommand::VmStart { id, attach } => {
            let vm_id = id as usize;
            if vm_id >= vmm::MAX_VMS {
                write_invalid_vm_id(endpoint, vm_id);
            } else {
                let vm = &mut vms[vm_id];
                match vm.state {
                    VmState::Running => {
                        let _ = write!(endpoint, "VM {} is already running.\r\n", vm_id);
                    }
                    VmState::Failed(_) => {
                        let _ = write!(
                            endpoint,
                            "ERR VM {} failed; use 'vm reset {}' to recover.\r\n",
                            vm_id, vm_id
                        );
                    }
                    VmState::Created => {
                        vm.state = VmState::Running;
                        let _ = write!(endpoint, "VM {} started.\r\n", vm_id);
                        info!("VM {} started by management shell", vm_id);
                    }
                    VmState::Stopped => {
                        vm.state = VmState::Running;
                        let _ = write!(endpoint, "VM {} resumed.\r\n", vm_id);
                        info!("VM {} resumed by management shell", vm_id);
                    }
                    VmState::NotCreated => {
                        let _ = write!(
                            endpoint,
                            "ERR VM {} does not exist; use 'vm create {} {}M' first.\r\n",
                            vm_id,
                            vm_id,
                            vmm::DEFAULT_GUEST_MEMORY_MIB
                        );
                    }
                }

                if attach && matches!(vm.state, VmState::Running) {
                    if attach_allowed {
                        serial::reset_guest_bridge();
                        let _ = write!(
                            endpoint,
                            "Attached to VM {} serial. Press Ctrl-] to return to the management shell.\r\n",
                            vm_id
                        );
                        endpoint.set_serial_attached(true);
                        serial_action = SerialAction::Attach(vm_id);
                        add_prompt = false;
                    } else {
                        endpoint.write_bytes(
                            b"ERR a VM serial is attached to another console or VM.\r\n",
                        );
                    }
                }
            }
        }
        ManagementCommand::VmStop { id } => {
            let vm_id = id as usize;
            if vm_id >= vmm::MAX_VMS {
                write_invalid_vm_id(endpoint, vm_id);
            } else if matches!(vms[vm_id].state, VmState::Running) {
                vms[vm_id].state = VmState::Stopped;
                let _ = write!(
                    endpoint,
                    "VM {} stopped; guest memory is retained.\r\n",
                    vm_id
                );
                info!("VM {} stopped by management shell", vm_id);
            } else {
                let _ = write!(endpoint, "VM {} is not running.\r\n", vm_id);
            }
        }
        ManagementCommand::VmReset { id } => {
            let vm_id = id as usize;
            if vm_id >= vmm::MAX_VMS {
                write_invalid_vm_id(endpoint, vm_id);
            } else {
                let vm = &mut vms[vm_id];
                let result = if matches!(vm.state, VmState::NotCreated) {
                    Err("VM does not exist; use 'vm create [ID] MEMORY' first")
                } else if let Some(vcpu) = vm.vcpu.as_mut() {
                    vcpu.reset()
                } else {
                    Err("VM construction previously failed; reboot the hypervisor to retry")
                };
                match result {
                    Ok(()) => {
                        vm.state = VmState::Running;
                        let _ = write!(endpoint, "VM {} reset and started.\r\n", vm_id);
                        info!("VM {} reset by management shell", vm_id);
                    }
                    Err(error) => {
                        vm.state = VmState::Failed(error);
                        let _ = write!(endpoint, "ERR unable to reset VM {}: {}\r\n", vm_id, error);
                    }
                }
            }
        }
        ManagementCommand::VmStatus { id } => {
            let vm_id = id as usize;
            if let Some(vm) = vms.get(vm_id) {
                write_vm_status(endpoint, vm_id, vm);
            } else {
                write_invalid_vm_id(endpoint, vm_id);
            }
        }
        ManagementCommand::SerialAttach { id } => {
            let vm_id = id as usize;
            if vm_id >= vmm::MAX_VMS {
                write_invalid_vm_id(endpoint, vm_id);
            } else if !matches!(vms[vm_id].state, VmState::Running) {
                let _ = write!(
                    endpoint,
                    "ERR VM {} serial is available only while running.\r\n",
                    vm_id
                );
            } else if !attach_allowed {
                endpoint.write_bytes(b"ERR a VM serial is attached to another console or VM.\r\n");
            } else {
                serial::reset_guest_bridge();
                let _ = write!(
                    endpoint,
                    "Attached to VM {} serial. Press Ctrl-] to return to the management shell.\r\n",
                    vm_id
                );
                endpoint.set_serial_attached(true);
                serial_action = SerialAction::Attach(vm_id);
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
            write_vm_list(endpoint, vms);
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

fn write_invalid_vm_id<E: ManagementEndpoint>(endpoint: &mut E, vm_id: usize) {
    let _ = write!(
        endpoint,
        "ERR VM ID {} is out of range; valid IDs are 0-{}.\r\n",
        vm_id,
        vmm::MAX_VMS - 1
    );
}
