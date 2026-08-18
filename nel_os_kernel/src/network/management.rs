use core::mem;

use crate::management::{BANNER, HELP, ManagementCommand, PROMPT, parse_command};

#[cfg(not(test))]
use super::CONTROL_PORT;
#[cfg(test)]
const CONTROL_PORT: u16 = 5555;

const ETHERNET_HEADER_LEN: usize = 14;
const IPV4_HEADER_LEN: usize = 20;
const TCP_HEADER_LEN: usize = 20;
const TCP_MAX_PAYLOAD: usize = 1024;
const TCP_RETRANSMIT_MILLIS: u64 = 500;
const TCP_MAX_RETRANSMITS: u8 = 5;
const TCP_SYN_COOKIE_PERIOD_MILLIS: u64 = 30_000;
const TCP_CLOSE_TIMEOUT_MILLIS: u64 = 10_000;
const TCP_IDLE_TIMEOUT_MILLIS: u64 = 15 * 60 * 1000;

const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TcpState {
    Closed,
    Established,
    CloseWait,
    LastAck,
}

struct ByteQueue<const N: usize> {
    bytes: [u8; N],
    head: usize,
    len: usize,
}

impl<const N: usize> ByteQueue<N> {
    const fn new() -> Self {
        Self {
            bytes: [0; N],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.bytes[(self.head + self.len) % N] = byte;
        self.len += 1;
        true
    }

    fn push_slice(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for &byte in bytes {
            if !self.push(byte) {
                break;
            }
            written += 1;
        }
        written
    }

    fn pop_into(&mut self, output: &mut [u8]) -> usize {
        let count = output.len().min(self.len);
        for slot in &mut output[..count] {
            *slot = self.bytes[self.head];
            self.head = (self.head + 1) % N;
        }
        self.len -= count;
        count
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(byte)
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn len(&self) -> usize {
        self.len
    }

    fn remaining(&self) -> usize {
        N - self.len
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

struct CommandQueue {
    commands: [Option<ManagementCommand>; 8],
    head: usize,
    len: usize,
}

impl CommandQueue {
    const fn new() -> Self {
        Self {
            commands: [None; 8],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, command: ManagementCommand) -> bool {
        if self.len == self.commands.len() {
            return false;
        }
        let index = (self.head + self.len) % self.commands.len();
        self.commands[index] = Some(command);
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<ManagementCommand> {
        if self.len == 0 {
            return None;
        }
        let command = self.commands[self.head].take();
        self.head = (self.head + 1) % self.commands.len();
        self.len -= 1;
        command
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.commands.fill(None);
    }
}

pub struct ManagementServer {
    state: TcpState,
    peer_mac: [u8; 6],
    peer_ip: [u8; 4],
    peer_port: u16,
    receive_next: u32,
    send_next: u32,
    unacked: bool,
    unacked_sequence: u32,
    unacked_flags: u8,
    unacked_payload: [u8; TCP_MAX_PAYLOAD],
    unacked_payload_len: usize,
    unacked_sent_at: u64,
    unacked_retransmits: u8,
    syn_secret: u64,
    last_activity_at: u64,
    ack_pending: bool,
    close_requested: bool,
    serial_attached: bool,
    output: ByteQueue<8192>,
    serial_input: ByteQueue<2048>,
    pending_input: ByteQueue<2048>,
    line: [u8; 256],
    line_len: usize,
    last_input_was_cr: bool,
    commands: CommandQueue,
}

impl ManagementServer {
    pub const fn new() -> Self {
        Self {
            state: TcpState::Closed,
            peer_mac: [0; 6],
            peer_ip: [0; 4],
            peer_port: 0,
            receive_next: 0,
            send_next: 0,
            unacked: false,
            unacked_sequence: 0,
            unacked_flags: 0,
            unacked_payload: [0; TCP_MAX_PAYLOAD],
            unacked_payload_len: 0,
            unacked_sent_at: 0,
            unacked_retransmits: 0,
            syn_secret: 0,
            last_activity_at: 0,
            ack_pending: false,
            close_requested: false,
            serial_attached: false,
            output: ByteQueue::new(),
            serial_input: ByteQueue::new(),
            pending_input: ByteQueue::new(),
            line: [0; 256],
            line_len: 0,
            last_input_was_cr: false,
            commands: CommandQueue::new(),
        }
    }

    pub fn take_command(&mut self) -> Option<ManagementCommand> {
        if let Some(command) = self.commands.pop() {
            return Some(command);
        }
        self.process_pending_input();
        self.commands.pop()
    }

    pub fn write(&mut self, bytes: &[u8]) -> usize {
        if !matches!(self.state, TcpState::Established | TcpState::CloseWait) {
            return 0;
        }
        self.output.push_slice(bytes)
    }

    pub fn write_serial(&mut self, bytes: &[u8]) -> usize {
        if !self.serial_attached {
            return 0;
        }
        self.write(bytes)
    }

    pub fn serial_output_capacity(&self) -> usize {
        if self.serial_attached {
            self.output.remaining()
        } else {
            0
        }
    }

    pub fn take_serial_input(&mut self, output: &mut [u8]) -> usize {
        self.serial_input.pop_into(output)
    }

    pub fn discard_serial_input(&mut self) -> usize {
        let discarded = self.serial_input.len();
        self.serial_input.clear();
        discarded
    }

    pub fn set_serial_attached(&mut self, attached: bool) {
        self.serial_attached = attached && self.state == TcpState::Established;
        if !self.serial_attached {
            self.serial_input.clear();
            // Bytes deferred after an attach command were accepted as guest
            // input. A lifecycle-driven detach must discard them rather than
            // reinterpret them as management commands.
            self.pending_input.clear();
            self.line_len = 0;
            self.last_input_was_cr = false;
        }
    }

    pub fn serial_attached(&self) -> bool {
        self.serial_attached
    }

    pub fn prompt(&mut self) {
        self.write(PROMPT);
    }

    pub fn notify_detach_or_close(&mut self, notice: &[u8]) -> bool {
        let required = notice.len().saturating_add(PROMPT.len());
        if !matches!(self.state, TcpState::Established | TcpState::CloseWait)
            || self.output.remaining() < required
        {
            self.request_close();
            return false;
        }
        let notice_written = self.output.push_slice(notice);
        let prompt_written = self.output.push_slice(PROMPT);
        debug_assert_eq!(notice_written, notice.len());
        debug_assert_eq!(prompt_written, PROMPT.len());
        true
    }

    pub fn request_close(&mut self) {
        self.close_requested = true;
        self.serial_attached = false;
    }

    pub(super) fn reset_connection(&mut self) {
        self.reset();
    }

    pub fn poll(
        &mut self,
        local_mac: [u8; 6],
        local_ip: [u8; 4],
        now: usize,
        response: &mut [u8],
    ) -> Option<usize> {
        let now = management_clock_millis(now);
        let (since, timeout) = match self.state {
            TcpState::CloseWait | TcpState::LastAck => {
                (self.last_activity_at, TCP_CLOSE_TIMEOUT_MILLIS)
            }
            TcpState::Established => (self.last_activity_at, TCP_IDLE_TIMEOUT_MILLIS),
            TcpState::Closed => (now, 0),
        };
        if timeout != 0 && now.wrapping_sub(since) >= timeout {
            self.reset();
            return None;
        }
        self.emit(local_mac, local_ip, now, response)
    }

    pub fn handle_ipv4(
        &mut self,
        frame: &[u8],
        ip_header_len: usize,
        ip_total_len: usize,
        local_mac: [u8; 6],
        local_ip: [u8; 4],
        now: usize,
        response: &mut [u8],
    ) -> Option<usize> {
        let now = management_clock_millis(now);
        let tcp_offset = ETHERNET_HEADER_LEN.checked_add(ip_header_len)?;
        let tcp_len = ip_total_len.checked_sub(ip_header_len)?;
        let frame_len = ETHERNET_HEADER_LEN.checked_add(ip_total_len)?;
        if tcp_len < TCP_HEADER_LEN || frame.len() < frame_len {
            return None;
        }
        let tcp = &frame[tcp_offset..frame_len];
        let tcp_header_len = ((tcp[12] >> 4) as usize).checked_mul(4)?;
        if tcp_header_len < TCP_HEADER_LEN || tcp_header_len > tcp.len() {
            return None;
        }
        let source_ip: [u8; 4] = frame[ETHERNET_HEADER_LEN + 12..ETHERNET_HEADER_LEN + 16]
            .try_into()
            .ok()?;
        let destination_ip: [u8; 4] = frame[ETHERNET_HEADER_LEN + 16..ETHERNET_HEADER_LEN + 20]
            .try_into()
            .ok()?;
        if destination_ip != local_ip || tcp_checksum(source_ip, destination_ip, tcp) != 0 {
            return None;
        }
        let source_port = read_u16(tcp, 0)?;
        let destination_port = read_u16(tcp, 2)?;
        if destination_port != CONTROL_PORT {
            return None;
        }
        let sequence = read_u32(tcp, 4)?;
        let acknowledgement = read_u32(tcp, 8)?;
        let flags = tcp[13];
        let payload = &tcp[tcp_header_len..];
        let source_mac: [u8; 6] = frame[6..12].try_into().ok()?;

        if flags & TCP_RST != 0 {
            if self.matches_peer(source_mac, source_ip, source_port)
                && sequence == self.receive_next
            {
                self.reset();
            }
            return None;
        }

        let is_initial_syn = flags & TCP_SYN != 0 && flags & TCP_ACK == 0 && payload.is_empty();
        if self.state == TcpState::Closed {
            let secret = self.ensure_syn_secret(now, local_mac);
            if is_initial_syn {
                let cookie = syn_cookie(
                    secret,
                    now / TCP_SYN_COOKIE_PERIOD_MILLIS,
                    source_ip,
                    source_port,
                    sequence,
                    source_mac,
                );
                return build_tcp_segment(
                    local_mac,
                    local_ip,
                    source_mac,
                    source_ip,
                    source_port,
                    cookie,
                    sequence.wrapping_add(1),
                    TCP_SYN | TCP_ACK,
                    &[],
                    2048,
                    response,
                );
            }

            if flags & TCP_ACK == 0 || flags & (TCP_SYN | TCP_FIN) != 0 || !payload.is_empty() {
                return None;
            }
            let peer_sequence = sequence.wrapping_sub(1);
            let cookie_period = now / TCP_SYN_COOKIE_PERIOD_MILLIS;
            let cookie_is_valid = [cookie_period, cookie_period.saturating_sub(1)]
                .into_iter()
                .any(|period| {
                    acknowledgement.wrapping_sub(1)
                        == syn_cookie(
                            secret,
                            period,
                            source_ip,
                            source_port,
                            peer_sequence,
                            source_mac,
                        )
                });
            if !cookie_is_valid {
                return None;
            }

            self.reset();
            self.peer_mac = source_mac;
            self.peer_ip = source_ip;
            self.peer_port = source_port;
            self.receive_next = sequence;
            self.send_next = acknowledgement;
            self.state = TcpState::Established;
            self.last_activity_at = now;
            self.output.push_slice(BANNER);
            return self.emit(local_mac, local_ip, now, response);
        }

        if !self.matches_peer(source_mac, source_ip, source_port) {
            return None;
        }

        if flags & TCP_SYN != 0 {
            self.ack_pending = true;
            return self.emit(local_mac, local_ip, now, response);
        }

        if flags & TCP_ACK != 0
            && sequence == self.receive_next
            && self.unacked
            && acknowledgement == self.send_next
        {
            self.last_activity_at = now;
            self.unacked = false;
            self.unacked_payload_len = 0;
            if self.state == TcpState::LastAck {
                self.reset();
                return None;
            }
        }

        if sequence == self.receive_next {
            if !payload.is_empty() {
                self.ack_pending = true;
                if self.state == TcpState::Established && !self.close_requested {
                    if !self.can_accept_input(payload) {
                        return self.emit(local_mac, local_ip, now, response);
                    }
                    self.last_activity_at = now;
                    self.receive_next = self.receive_next.wrapping_add(payload.len() as u32);
                    self.consume_input(payload);
                } else {
                    return self.emit(local_mac, local_ip, now, response);
                }
            } else if flags & TCP_ACK != 0 {
                self.last_activity_at = now;
            }
            if flags & TCP_FIN != 0 {
                self.last_activity_at = now;
                self.receive_next = self.receive_next.wrapping_add(1);
                self.ack_pending = true;
                self.serial_attached = false;
                self.state = TcpState::CloseWait;
                self.close_requested = true;
            }
        } else if !payload.is_empty() || flags & TCP_FIN != 0 {
            self.ack_pending = true;
        }

        self.emit(local_mac, local_ip, now, response)
    }

    fn ensure_syn_secret(&mut self, now: u64, local_mac: [u8; 6]) -> u64 {
        if self.syn_secret != 0 {
            return self.syn_secret;
        }
        #[cfg(not(test))]
        let entropy = unsafe { core::arch::x86_64::_rdtsc() };
        #[cfg(test)]
        let entropy = 0xd1b5_4a32_d192_ed03;

        let mut secret = entropy ^ now.rotate_left(23);
        for byte in local_mac {
            secret = mix64(secret ^ byte as u64);
        }
        self.syn_secret = secret | 1;
        self.syn_secret
    }

    fn consume_input(&mut self, bytes: &[u8]) {
        debug_assert!(bytes.len() <= self.pending_input.remaining());
        self.pending_input.push_slice(bytes);
        self.process_pending_input();
    }

    fn process_pending_input(&mut self) {
        while let Some(byte) = self.pending_input.pop() {
            // A CR-completed management command may enable attachment before
            // the LF arrives. Always swallow that LF as part of CRLF.
            if self.last_input_was_cr {
                self.last_input_was_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }

            if self.serial_attached {
                if byte == 0x1d {
                    self.serial_attached = false;
                    self.last_input_was_cr = true;
                    if !self.notify_detach_or_close(b"\r\nDetached from VM serial.\r\n") {
                        self.pending_input.clear();
                        break;
                    }
                } else {
                    let queued = self.serial_input.push(byte);
                    debug_assert!(queued);
                    if !queued {
                        // Capacity is checked before accepting the TCP payload.
                        // Close explicitly if that invariant is ever violated,
                        // instead of acknowledging input that cannot be kept.
                        self.pending_input.clear();
                        self.request_close();
                        break;
                    }
                }
                continue;
            }

            match byte {
                b'\r' => {
                    self.finish_or_prompt();
                    self.last_input_was_cr = true;
                    break;
                }
                b'\n' => {
                    self.finish_or_prompt();
                    break;
                }
                0x08 | 0x7f => {
                    self.line_len = self.line_len.saturating_sub(1);
                }
                byte if self.line_len < self.line.len() => {
                    self.line[self.line_len] = byte;
                    self.line_len += 1;
                }
                _ => {
                    self.line_len = 0;
                    self.output.push_slice(b"ERR command is too long\r\nnel> ");
                    break;
                }
            }
            if self.close_requested {
                break;
            }
        }
    }

    fn finish_or_prompt(&mut self) {
        if self.line_len == 0 {
            self.enqueue_command(ManagementCommand::Prompt);
            return;
        }
        self.finish_line();
        self.line_len = 0;
    }

    fn can_accept_input(&self, bytes: &[u8]) -> bool {
        if bytes.len() > self.pending_input.remaining() {
            return false;
        }
        if !self.serial_attached {
            return true;
        }

        let serial_bytes = bytes
            .iter()
            .position(|&byte| byte == 0x1d)
            .unwrap_or(bytes.len());
        serial_bytes <= self.serial_input.remaining()
    }

    fn finish_line(&mut self) {
        let command = parse_command(&self.line[..self.line_len]);
        self.enqueue_command(command);
    }

    fn enqueue_command(&mut self, command: ManagementCommand) {
        if !self.commands.push(command) {
            self.output
                .push_slice(b"ERR management queue is busy\r\nnel> ");
        } else if command == ManagementCommand::Disconnect {
            // Stop accepting command bytes as soon as `exit` is parsed.
            // The controller will enqueue all earlier responses before FIN.
            self.close_requested = true;
        }
    }

    pub fn write_help(&mut self) {
        self.write(HELP);
    }

    fn emit(
        &mut self,
        local_mac: [u8; 6],
        local_ip: [u8; 4],
        now: u64,
        response: &mut [u8],
    ) -> Option<usize> {
        if self.state == TcpState::Closed {
            return None;
        }
        if self.ack_pending {
            self.ack_pending = false;
            return self.build_segment(local_mac, local_ip, self.send_next, TCP_ACK, &[], response);
        }
        if self.unacked {
            if now.wrapping_sub(self.unacked_sent_at) < TCP_RETRANSMIT_MILLIS {
                return None;
            }
            if self.unacked_retransmits >= TCP_MAX_RETRANSMITS {
                self.reset();
                return None;
            }
            self.unacked_sent_at = now;
            self.unacked_retransmits += 1;
            return self.build_segment(
                local_mac,
                local_ip,
                self.unacked_sequence,
                self.unacked_flags,
                &self.unacked_payload[..self.unacked_payload_len],
                response,
            );
        }
        if !matches!(self.state, TcpState::Established | TcpState::CloseWait) {
            return None;
        }
        if !self.output.is_empty() {
            let mut payload = mem::replace(&mut self.unacked_payload, [0; TCP_MAX_PAYLOAD]);
            let payload_len = self.output.pop_into(&mut payload);
            self.unacked_payload = payload;
            self.start_unacked(TCP_PSH | TCP_ACK, &[], now);
            self.unacked_payload_len = payload_len;
            let length = self.build_segment(
                local_mac,
                local_ip,
                self.unacked_sequence,
                self.unacked_flags,
                &self.unacked_payload[..payload_len],
                response,
            );
            self.send_next = self.send_next.wrapping_add(payload_len as u32);
            return length;
        }
        if self.close_requested {
            self.state = TcpState::LastAck;
            self.start_unacked(TCP_FIN | TCP_ACK, &[], now);
            let length = self.build_segment(
                local_mac,
                local_ip,
                self.unacked_sequence,
                self.unacked_flags,
                &[],
                response,
            );
            self.send_next = self.send_next.wrapping_add(1);
            return length;
        }
        None
    }

    fn start_unacked(&mut self, flags: u8, payload: &[u8], now: u64) {
        self.unacked = true;
        self.unacked_sequence = self.send_next;
        self.unacked_flags = flags;
        self.unacked_payload_len = payload.len();
        self.unacked_payload[..payload.len()].copy_from_slice(payload);
        self.unacked_sent_at = now;
        self.unacked_retransmits = 0;
    }

    fn build_segment(
        &self,
        local_mac: [u8; 6],
        local_ip: [u8; 4],
        sequence: u32,
        flags: u8,
        payload: &[u8],
        output: &mut [u8],
    ) -> Option<usize> {
        let receive_window = if self.serial_attached {
            self.serial_input
                .remaining()
                .min(self.pending_input.remaining())
        } else {
            self.pending_input.remaining()
        };
        build_tcp_segment(
            local_mac,
            local_ip,
            self.peer_mac,
            self.peer_ip,
            self.peer_port,
            sequence,
            self.receive_next,
            flags,
            payload,
            receive_window,
            output,
        )
    }

    fn matches_peer(&self, mac: [u8; 6], ip: [u8; 4], port: u16) -> bool {
        self.peer_mac == mac && self.peer_ip == ip && self.peer_port == port
    }

    fn reset(&mut self) {
        self.state = TcpState::Closed;
        self.peer_mac = [0; 6];
        self.peer_ip = [0; 4];
        self.peer_port = 0;
        self.receive_next = 0;
        self.send_next = 0;
        self.unacked = false;
        self.unacked_payload_len = 0;
        self.unacked_retransmits = 0;
        self.last_activity_at = 0;
        self.ack_pending = false;
        self.close_requested = false;
        self.serial_attached = false;
        self.output.clear();
        self.serial_input.clear();
        self.pending_input.clear();
        self.line_len = 0;
        self.last_input_was_cr = false;
        self.commands.clear();
    }
}

#[allow(clippy::too_many_arguments)]
fn build_tcp_segment(
    local_mac: [u8; 6],
    local_ip: [u8; 4],
    peer_mac: [u8; 6],
    peer_ip: [u8; 4],
    peer_port: u16,
    sequence: u32,
    acknowledgement: u32,
    flags: u8,
    payload: &[u8],
    receive_window: usize,
    output: &mut [u8],
) -> Option<usize> {
    let tcp_len = TCP_HEADER_LEN.checked_add(payload.len())?;
    let frame_len = ETHERNET_HEADER_LEN
        .checked_add(IPV4_HEADER_LEN)?
        .checked_add(tcp_len)?;
    if output.len() < frame_len {
        return None;
    }
    output[..frame_len].fill(0);
    output[..6].copy_from_slice(&peer_mac);
    output[6..12].copy_from_slice(&local_mac);
    write_u16(output, 12, 0x0800)?;

    let ip = &mut output[ETHERNET_HEADER_LEN..frame_len];
    ip[0] = 0x45;
    write_u16(ip, 2, (IPV4_HEADER_LEN + tcp_len) as u16)?;
    ip[8] = 64;
    ip[9] = 6;
    ip[12..16].copy_from_slice(&local_ip);
    ip[16..20].copy_from_slice(&peer_ip);
    let ip_sum = checksum(&ip[..IPV4_HEADER_LEN]);
    write_u16(ip, 10, ip_sum)?;

    let tcp = &mut ip[IPV4_HEADER_LEN..];
    write_u16(tcp, 0, CONTROL_PORT)?;
    write_u16(tcp, 2, peer_port)?;
    write_u32(tcp, 4, sequence)?;
    write_u32(tcp, 8, acknowledgement)?;
    tcp[12] = 5 << 4;
    tcp[13] = flags;
    write_u16(tcp, 14, receive_window.min(u16::MAX as usize) as u16)?;
    tcp[TCP_HEADER_LEN..].copy_from_slice(payload);
    let tcp_sum = tcp_checksum(local_ip, peer_ip, tcp);
    write_u16(tcp, 16, tcp_sum)?;
    Some(frame_len)
}

#[cfg(not(test))]
fn management_clock_millis(fallback: usize) -> u64 {
    let Some(&tsc_khz) = crate::interrupt::apic::GUEST_TSC_KHZ.get() else {
        return fallback as u64;
    };
    if tsc_khz == 0 {
        return fallback as u64;
    }
    unsafe { core::arch::x86_64::_rdtsc() / tsc_khz }
}

#[cfg(test)]
fn management_clock_millis(fallback: usize) -> u64 {
    fallback as u64
}

fn syn_cookie(
    secret: u64,
    period: u64,
    peer_ip: [u8; 4],
    peer_port: u16,
    peer_sequence: u32,
    peer_mac: [u8; 6],
) -> u32 {
    let mut value = mix64(secret ^ period.rotate_left(17) ^ peer_sequence as u64);
    for byte in peer_ip.into_iter().chain(peer_mac) {
        value = mix64(value ^ byte as u64);
    }
    mix64(value ^ peer_port as u64) as u32
}

fn mix64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) -> Option<()> {
    bytes
        .get_mut(offset..offset + 2)?
        .copy_from_slice(&value.to_be_bytes());
    Some(())
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Option<()> {
    bytes
        .get_mut(offset..offset + 4)?
        .copy_from_slice(&value.to_be_bytes());
    Some(())
}

fn checksum(bytes: &[u8]) -> u16 {
    finish_sum(add_bytes(0, bytes))
}

fn tcp_checksum(source: [u8; 4], destination: [u8; 4], tcp: &[u8]) -> u16 {
    let mut sum = 0u32;
    sum = add_bytes(sum, &source);
    sum = add_bytes(sum, &destination);
    sum += 6;
    sum += tcp.len() as u32;
    finish_sum(add_bytes(sum, tcp))
}

fn add_bytes(mut sum: u32, bytes: &[u8]) -> u32 {
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        sum = sum.wrapping_add(u16::from_be_bytes([chunk[0], chunk[1]]) as u32);
    }
    if let Some(&last) = chunks.remainder().first() {
        sum = sum.wrapping_add((last as u32) << 8);
    }
    sum
}

fn finish_sum(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL_MAC: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
    const PEER_MAC: [u8; 6] = [0x52, 0x55, 0, 0x02, 0x00, 0x02];
    const LOCAL_IP: [u8; 4] = [10, 0, 2, 15];
    const PEER_IP: [u8; 4] = [10, 0, 2, 2];
    const PEER_PORT: u16 = 41000;

    fn client_segment(
        sequence: u32,
        acknowledgement: u32,
        flags: u8,
        payload: &[u8],
    ) -> ([u8; 1500], usize) {
        client_segment_from_port(PEER_PORT, sequence, acknowledgement, flags, payload)
    }

    fn client_segment_from_port(
        source_port: u16,
        sequence: u32,
        acknowledgement: u32,
        flags: u8,
        payload: &[u8],
    ) -> ([u8; 1500], usize) {
        let tcp_len = TCP_HEADER_LEN + payload.len();
        let frame_len = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN + tcp_len;
        let mut frame = [0u8; 1500];
        frame[..6].copy_from_slice(&LOCAL_MAC);
        frame[6..12].copy_from_slice(&PEER_MAC);
        write_u16(&mut frame, 12, 0x0800).unwrap();
        let ip = &mut frame[ETHERNET_HEADER_LEN..frame_len];
        ip[0] = 0x45;
        write_u16(ip, 2, (IPV4_HEADER_LEN + tcp_len) as u16).unwrap();
        ip[8] = 64;
        ip[9] = 6;
        ip[12..16].copy_from_slice(&PEER_IP);
        ip[16..20].copy_from_slice(&LOCAL_IP);
        let value = checksum(&ip[..IPV4_HEADER_LEN]);
        write_u16(ip, 10, value).unwrap();
        let tcp = &mut ip[IPV4_HEADER_LEN..];
        write_u16(tcp, 0, source_port).unwrap();
        write_u16(tcp, 2, CONTROL_PORT).unwrap();
        write_u32(tcp, 4, sequence).unwrap();
        write_u32(tcp, 8, acknowledgement).unwrap();
        tcp[12] = 5 << 4;
        tcp[13] = flags;
        write_u16(tcp, 14, 4096).unwrap();
        tcp[TCP_HEADER_LEN..].copy_from_slice(payload);
        let value = tcp_checksum(PEER_IP, LOCAL_IP, tcp);
        write_u16(tcp, 16, value).unwrap();
        (frame, frame_len)
    }

    #[test]
    fn byte_queue_wraps_without_reordering() {
        let mut queue = ByteQueue::<4>::new();
        assert_eq!(queue.push_slice(b"abc"), 3);
        let mut first = [0; 2];
        assert_eq!(queue.pop_into(&mut first), 2);
        assert_eq!(&first, b"ab");
        assert_eq!(queue.push_slice(b"de"), 2);
        let mut rest = [0; 3];
        assert_eq!(queue.pop_into(&mut rest), 3);
        assert_eq!(&rest, b"cde");
    }

    #[test]
    fn parses_management_commands() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.consume_input(b"vm start\r\nvm start --attach\nvm info\ninfo memory\n");
        assert_eq!(server.take_command(), Some(ManagementCommand::VmStart));
        assert_eq!(
            server.take_command(),
            Some(ManagementCommand::VmStartAttach)
        );
        assert_eq!(server.take_command(), Some(ManagementCommand::VmStatus));
        assert_eq!(server.take_command(), Some(ManagementCommand::InfoMemory));
    }

    #[test]
    fn empty_enter_prints_one_prompt_per_line_ending() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;

        server.consume_input(b"\r\n");
        assert_eq!(server.take_command(), Some(ManagementCommand::Prompt));
        assert!(server.output.is_empty());

        server.consume_input(b"\n");
        assert_eq!(server.take_command(), Some(ManagementCommand::Prompt));
        assert!(server.output.is_empty());
    }

    #[test]
    fn control_bracket_detaches_serial() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.serial_attached = true;
        server.consume_input(b"a\x1db");
        assert!(!server.serial_attached());
        let mut input = [0; 4];
        assert_eq!(server.take_serial_input(&mut input), 1);
        assert_eq!(input[0], b'a');
    }

    #[test]
    fn defers_bytes_after_attach_command_until_mode_changes() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.consume_input(b"serial attach\r\nlogin\n");

        assert_eq!(server.take_command(), Some(ManagementCommand::SerialAttach));
        server.set_serial_attached(true);
        assert_eq!(server.take_command(), None);

        let mut input = [0; 16];
        let count = server.take_serial_input(&mut input);
        assert_eq!(&input[..count], b"login\n");
    }

    #[test]
    fn forced_detach_discards_deferred_guest_directed_input() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.consume_input(b"serial attach\nvm reset\n");
        assert_eq!(server.take_command(), Some(ManagementCommand::SerialAttach));

        server.set_serial_attached(true);
        server.set_serial_attached(false);
        assert_eq!(server.take_command(), None);
        let mut input = [0; 16];
        assert_eq!(server.take_serial_input(&mut input), 0);
    }

    #[test]
    fn full_output_queue_closes_instead_of_partially_writing_detach_notice() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        assert_eq!(server.output.push_slice(&[b'x'; 8192]), 8192);

        assert!(!server.notify_detach_or_close(b"detached\r\n"));
        assert!(server.close_requested);
        assert_eq!(server.output.remaining(), 0);
    }

    #[test]
    fn retransmitted_syn_gets_original_syn_ack_immediately() {
        let mut server = ManagementServer::new();
        let mut response = [0u8; 1500];
        let (syn, syn_len) = client_segment(100, 0, TCP_SYN, &[]);
        let first_len = server
            .handle_ipv4(
                &syn[..syn_len],
                IPV4_HEADER_LEN,
                syn_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                1,
                &mut response,
            )
            .unwrap();
        let first_sequence = read_u32(
            &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..first_len],
            4,
        );

        let retry_len = server
            .handle_ipv4(
                &syn[..syn_len],
                IPV4_HEADER_LEN,
                syn_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                2,
                &mut response,
            )
            .unwrap();
        let retry = &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..retry_len];
        assert_eq!(retry[13], TCP_SYN | TCP_ACK);
        assert_eq!(read_u32(retry, 4), first_sequence);
    }

    #[test]
    fn syn_cookies_do_not_reserve_a_half_open_slot() {
        let mut server = ManagementServer::new();
        let mut response = [0u8; 1500];
        let (first, first_len) = client_segment(100, 0, TCP_SYN, &[]);
        let first_syn_ack_len = server
            .handle_ipv4(
                &first[..first_len],
                IPV4_HEADER_LEN,
                first_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                1,
                &mut response,
            )
            .unwrap();
        let first_cookie = read_u32(
            &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..first_syn_ack_len],
            4,
        )
        .unwrap();
        assert_eq!(server.state, TcpState::Closed);

        let replacement_port = PEER_PORT + 1;
        let (replacement, replacement_len) =
            client_segment_from_port(replacement_port, 200, 0, TCP_SYN, &[]);
        assert!(
            server
                .handle_ipv4(
                    &replacement[..replacement_len],
                    IPV4_HEADER_LEN,
                    replacement_len - ETHERNET_HEADER_LEN,
                    LOCAL_MAC,
                    LOCAL_IP,
                    2,
                    &mut response,
                )
                .is_some()
        );
        assert_eq!(server.state, TcpState::Closed);

        let (first_ack, first_ack_len) =
            client_segment(101, first_cookie.wrapping_add(1), TCP_ACK, &[]);
        assert!(
            server
                .handle_ipv4(
                    &first_ack[..first_ack_len],
                    IPV4_HEADER_LEN,
                    first_ack_len - ETHERNET_HEADER_LEN,
                    LOCAL_MAC,
                    LOCAL_IP,
                    3,
                    &mut response,
                )
                .is_some()
        );
        assert_eq!(server.state, TcpState::Established);
        assert_eq!(server.peer_port, PEER_PORT);
    }

    #[test]
    fn cookie_from_previous_period_is_accepted() {
        let mut server = ManagementServer::new();
        let mut response = [0u8; 1500];
        let syn_time = TCP_SYN_COOKIE_PERIOD_MILLIS as usize - 1;
        let (syn, syn_len) = client_segment(100, 0, TCP_SYN, &[]);
        let syn_ack_len = server
            .handle_ipv4(
                &syn[..syn_len],
                IPV4_HEADER_LEN,
                syn_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                syn_time,
                &mut response,
            )
            .unwrap();
        let cookie = read_u32(
            &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..syn_ack_len],
            4,
        )
        .unwrap();
        let (ack, ack_len) = client_segment(101, cookie.wrapping_add(1), TCP_ACK, &[]);
        assert!(
            server
                .handle_ipv4(
                    &ack[..ack_len],
                    IPV4_HEADER_LEN,
                    ack_len - ETHERNET_HEADER_LEN,
                    LOCAL_MAC,
                    LOCAL_IP,
                    TCP_SYN_COOKIE_PERIOD_MILLIS as usize,
                    &mut response,
                )
                .is_some()
        );
        assert_eq!(server.state, TcpState::Established);
    }

    #[test]
    fn invalid_cookie_does_not_allocate_connection_state() {
        let mut server = ManagementServer::new();
        let mut response = [0u8; 1500];
        let (invalid, invalid_len) = client_segment(101, 123, TCP_ACK, &[]);
        assert_eq!(
            server.handle_ipv4(
                &invalid[..invalid_len],
                IPV4_HEADER_LEN,
                invalid_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                1,
                &mut response,
            ),
            None
        );
        assert_eq!(server.state, TcpState::Closed);
    }

    #[test]
    fn tcp_handshake_delivers_banner_and_command() {
        let mut server = ManagementServer::new();
        let mut response = [0u8; 1500];
        let (syn, syn_len) = client_segment(100, 0, TCP_SYN, &[]);
        let syn_ack_len = server
            .handle_ipv4(
                &syn[..syn_len],
                IPV4_HEADER_LEN,
                syn_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                7,
                &mut response,
            )
            .unwrap();
        let syn_ack = &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..syn_ack_len];
        assert_eq!(syn_ack[13], TCP_SYN | TCP_ACK);
        assert_eq!(read_u32(syn_ack, 8), Some(101));
        let server_sequence = read_u32(syn_ack, 4).unwrap();
        assert_eq!(tcp_checksum(LOCAL_IP, PEER_IP, syn_ack), 0);

        let (ack, ack_len) = client_segment(101, server_sequence + 1, TCP_ACK, &[]);
        let banner_len = server
            .handle_ipv4(
                &ack[..ack_len],
                IPV4_HEADER_LEN,
                ack_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                8,
                &mut response,
            )
            .unwrap();
        let banner_tcp = &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..banner_len];
        assert_eq!(&banner_tcp[TCP_HEADER_LEN..], BANNER);
        assert_eq!(tcp_checksum(LOCAL_IP, PEER_IP, banner_tcp), 0);

        let banner_end = server_sequence + 1 + BANNER.len() as u32;
        let (banner_ack, banner_ack_len) = client_segment(101, banner_end, TCP_ACK, &[]);
        assert_eq!(
            server.handle_ipv4(
                &banner_ack[..banner_ack_len],
                IPV4_HEADER_LEN,
                banner_ack_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                9,
                &mut response,
            ),
            None
        );

        let command = b"vm status\n";
        let (request, request_len) = client_segment(101, banner_end, TCP_ACK, command);
        let ack_len = server
            .handle_ipv4(
                &request[..request_len],
                IPV4_HEADER_LEN,
                request_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                10,
                &mut response,
            )
            .unwrap();
        let response_tcp = &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..ack_len];
        assert_eq!(response_tcp[13], TCP_ACK);
        assert_eq!(read_u32(response_tcp, 8), Some(101 + command.len() as u32));
        assert_eq!(server.take_command(), Some(ManagementCommand::VmStatus));
    }

    #[test]
    fn tcp_half_close_preserves_final_command() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.peer_mac = PEER_MAC;
        server.peer_ip = PEER_IP;
        server.peer_port = PEER_PORT;
        server.receive_next = 7;
        server.send_next = 10;
        let command = b"vm status\n";
        let (request, request_len) = client_segment(7, 10, TCP_ACK | TCP_FIN, command);
        let mut response = [0u8; 1500];
        assert!(
            server
                .handle_ipv4(
                    &request[..request_len],
                    IPV4_HEADER_LEN,
                    request_len - ETHERNET_HEADER_LEN,
                    LOCAL_MAC,
                    LOCAL_IP,
                    1,
                    &mut response,
                )
                .is_some()
        );
        assert_eq!(server.state, TcpState::CloseWait);
        assert_eq!(server.take_command(), Some(ManagementCommand::VmStatus));
    }

    #[test]
    fn established_idle_timeout_releases_connection_slot() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.peer_mac = PEER_MAC;
        server.peer_ip = PEER_IP;
        server.peer_port = PEER_PORT;
        server.last_activity_at = 10;
        let mut response = [0u8; 1500];

        assert_eq!(
            server.poll(
                LOCAL_MAC,
                LOCAL_IP,
                10 + TCP_IDLE_TIMEOUT_MILLIS as usize,
                &mut response,
            ),
            None
        );
        assert_eq!(server.state, TcpState::Closed);
    }

    #[test]
    fn retransmit_limit_releases_connection_slot() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.peer_mac = PEER_MAC;
        server.peer_ip = PEER_IP;
        server.peer_port = PEER_PORT;
        server.receive_next = 10;
        server.send_next = 20;
        server.start_unacked(TCP_PSH | TCP_ACK, b"x", 0);
        let mut response = [0u8; 1500];

        for retry in 1..=TCP_MAX_RETRANSMITS {
            assert!(
                server
                    .poll(
                        LOCAL_MAC,
                        LOCAL_IP,
                        retry as usize * TCP_RETRANSMIT_MILLIS as usize,
                        &mut response,
                    )
                    .is_some()
            );
        }
        assert_eq!(
            server.poll(
                LOCAL_MAC,
                LOCAL_IP,
                (TCP_MAX_RETRANSMITS as usize + 1) * TCP_RETRANSMIT_MILLIS as usize,
                &mut response,
            ),
            None
        );
        assert_eq!(server.state, TcpState::Closed);
    }

    #[test]
    fn wrong_sequence_cannot_ack_handshake_or_reset_connection() {
        let mut server = ManagementServer::new();
        let mut response = [0u8; 1500];
        let (syn, syn_len) = client_segment(100, 0, TCP_SYN, &[]);
        let syn_ack_len = server
            .handle_ipv4(
                &syn[..syn_len],
                IPV4_HEADER_LEN,
                syn_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                1,
                &mut response,
            )
            .unwrap();
        let syn_ack = &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..syn_ack_len];
        let server_sequence = read_u32(syn_ack, 4).unwrap();
        let (bad_ack, bad_ack_len) = client_segment(999, server_sequence + 1, TCP_ACK, &[]);
        assert_eq!(
            server.handle_ipv4(
                &bad_ack[..bad_ack_len],
                IPV4_HEADER_LEN,
                bad_ack_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                2,
                &mut response,
            ),
            None
        );
        assert_eq!(server.state, TcpState::Closed);

        let (ack, ack_len) = client_segment(101, server_sequence + 1, TCP_ACK, &[]);
        server.handle_ipv4(
            &ack[..ack_len],
            IPV4_HEADER_LEN,
            ack_len - ETHERNET_HEADER_LEN,
            LOCAL_MAC,
            LOCAL_IP,
            3,
            &mut response,
        );
        assert_eq!(server.state, TcpState::Established);
        let (bad_rst, bad_rst_len) = client_segment(999, 0, TCP_RST, &[]);
        assert_eq!(
            server.handle_ipv4(
                &bad_rst[..bad_rst_len],
                IPV4_HEADER_LEN,
                bad_rst_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                4,
                &mut response,
            ),
            None
        );
        assert_eq!(server.state, TcpState::Established);
    }

    #[test]
    fn wrong_sequence_does_not_refresh_idle_timeout() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.peer_mac = PEER_MAC;
        server.peer_ip = PEER_IP;
        server.peer_port = PEER_PORT;
        server.receive_next = 50;
        server.send_next = 100;
        server.last_activity_at = 1;
        let mut response = [0u8; 1500];
        let (invalid, invalid_len) = client_segment(999, 100, TCP_ACK, &[]);

        assert_eq!(
            server.handle_ipv4(
                &invalid[..invalid_len],
                IPV4_HEADER_LEN,
                invalid_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                TCP_IDLE_TIMEOUT_MILLIS as usize,
                &mut response,
            ),
            None
        );
        assert_eq!(server.last_activity_at, 1);
        assert_eq!(
            server.poll(
                LOCAL_MAC,
                LOCAL_IP,
                1 + TCP_IDLE_TIMEOUT_MILLIS as usize,
                &mut response,
            ),
            None
        );
        assert_eq!(server.state, TcpState::Closed);
    }

    #[test]
    fn exit_rejects_following_commands_in_same_segment() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.consume_input(b"exit\nvm reset\n");
        assert_eq!(server.take_command(), Some(ManagementCommand::Disconnect));
        assert_eq!(server.take_command(), None);
        assert!(server.close_requested);
    }

    #[test]
    fn serial_input_uses_tcp_backpressure_without_losing_bytes() {
        let mut server = ManagementServer::new();
        server.state = TcpState::Established;
        server.peer_mac = PEER_MAC;
        server.peer_ip = PEER_IP;
        server.peer_port = PEER_PORT;
        server.receive_next = 50;
        server.send_next = 100;
        server.serial_attached = true;
        assert_eq!(server.serial_input.push_slice(&[b'x'; 2048]), 2048);

        let (request, request_len) = client_segment(50, 100, TCP_ACK, b"abc");
        let mut response = [0u8; 1500];
        let response_len = server
            .handle_ipv4(
                &request[..request_len],
                IPV4_HEADER_LEN,
                request_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                1,
                &mut response,
            )
            .unwrap();
        let response_tcp = &response[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN..response_len];
        assert_eq!(read_u32(response_tcp, 8), Some(50));
        assert_eq!(read_u16(response_tcp, 14), Some(0));

        let mut drain = [0u8; 2048];
        assert_eq!(server.take_serial_input(&mut drain), 2048);
        server
            .handle_ipv4(
                &request[..request_len],
                IPV4_HEADER_LEN,
                request_len - ETHERNET_HEADER_LEN,
                LOCAL_MAC,
                LOCAL_IP,
                2,
                &mut response,
            )
            .unwrap();
        assert_eq!(server.receive_next, 53);
        let mut accepted = [0u8; 3];
        assert_eq!(server.take_serial_input(&mut accepted), 3);
        assert_eq!(&accepted, b"abc");
    }
}
