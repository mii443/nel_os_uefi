pub const CONTROL_PORT: u16 = 5555;

const ETHERNET_HEADER_LEN: usize = 14;
const ARP_PACKET_LEN: usize = 28;
const IPV4_HEADER_LEN: usize = 20;
const UDP_HEADER_LEN: usize = 8;

const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERTYPE_IPV4: u16 = 0x0800;
const IPV4_BROADCAST: [u8; 4] = [255; 4];
const IPV4_UNSPECIFIED: [u8; 4] = [0; 4];

const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_FIXED_LEN: usize = 240;
const DHCP_MIN_MESSAGE_LEN: usize = 300;
const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;
const DHCP_NAK: u8 = 6;
const DHCP_RETRY_MILLIS: usize = 4_000;
const DEFAULT_LEASE_SECONDS: u32 = 3600;
const MILLIS_PER_SECOND: usize = 1_000;

const START_COMMAND: &[u8] = b"start";
const START_RESPONSE: &[u8] = b"OK starting Linux VM\n";
const COMMAND_ERROR_RESPONSE: &[u8] = b"ERR send: start\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Config {
    pub address: [u8; 4],
    pub subnet_mask: [u8; 4],
    pub router: [u8; 4],
    pub dhcp_server: [u8; 4],
    pub lease_seconds: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DhcpState {
    Init,
    Selecting,
    Requesting,
    Bound,
    Renewing,
    Rebinding,
}

#[derive(Default)]
struct DhcpOptions {
    message_type: Option<u8>,
    server: Option<[u8; 4]>,
    subnet_mask: Option<[u8; 4]>,
    router: Option<[u8; 4]>,
    lease_seconds: Option<u32>,
    renewal_seconds: Option<u32>,
    rebinding_seconds: Option<u32>,
}

pub struct NetworkStack {
    mac: [u8; 6],
    xid: u32,
    dhcp_state: DhcpState,
    requested_address: [u8; 4],
    dhcp_server: [u8; 4],
    next_dhcp_tick: usize,
    lease_renewal_tick: usize,
    lease_rebinding_tick: usize,
    lease_expiry_tick: usize,
    config: Option<Ipv4Config>,
    start_requested: bool,
    management: super::management::ManagementListener,
}

impl NetworkStack {
    pub const fn new(mac: [u8; 6]) -> Self {
        let xid = 0x4e45_4c00
            ^ ((mac[2] as u32) << 24)
            ^ ((mac[3] as u32) << 16)
            ^ ((mac[4] as u32) << 8)
            ^ mac[5] as u32;
        Self {
            mac,
            xid,
            dhcp_state: DhcpState::Init,
            requested_address: IPV4_UNSPECIFIED,
            dhcp_server: IPV4_UNSPECIFIED,
            next_dhcp_tick: 0,
            lease_renewal_tick: 0,
            lease_rebinding_tick: 0,
            lease_expiry_tick: 0,
            config: None,
            start_requested: false,
            management: super::management::ManagementListener::new(),
        }
    }

    pub fn ipv4_config(&self) -> Option<Ipv4Config> {
        self.config
    }

    pub fn take_start_request(&mut self) -> bool {
        core::mem::take(&mut self.start_requested)
    }

    pub fn take_management_command(
        &mut self,
    ) -> Option<(super::management::ConnectionId, super::ManagementCommand)> {
        self.management.take_command()
    }

    pub fn write_management(&mut self, id: super::management::ConnectionId, bytes: &[u8]) -> usize {
        self.management.write(id, bytes)
    }

    pub fn management_prompt(&mut self, id: super::management::ConnectionId) {
        self.management.prompt(id);
    }

    pub fn notify_management_detach_or_close(
        &mut self,
        id: super::management::ConnectionId,
        notice: &[u8],
    ) -> bool {
        self.management.notify_detach_or_close(id, notice)
    }

    pub fn write_management_help(&mut self, id: super::management::ConnectionId) {
        self.management.write_help(id);
    }

    pub fn request_management_close(&mut self, id: super::management::ConnectionId) {
        self.management.request_close(id);
    }

    pub fn set_serial_attached(&mut self, id: super::management::ConnectionId, attached: bool) {
        self.management.set_serial_attached(id, attached);
    }

    pub fn serial_attached(&self, id: super::management::ConnectionId) -> bool {
        self.management.serial_attached(id)
    }

    pub fn take_serial_input(
        &mut self,
        id: super::management::ConnectionId,
        output: &mut [u8],
    ) -> usize {
        self.management.take_serial_input(id, output)
    }

    pub fn discard_serial_input(&mut self, id: super::management::ConnectionId) -> usize {
        self.management.discard_serial_input(id)
    }

    pub fn write_serial_output(
        &mut self,
        id: super::management::ConnectionId,
        bytes: &[u8],
    ) -> usize {
        self.management.write_serial(id, bytes)
    }

    pub fn serial_output_capacity(&self, id: super::management::ConnectionId) -> usize {
        self.management.serial_output_capacity(id)
    }

    pub fn poll(&mut self, now: usize, output: &mut [u8]) -> Option<usize> {
        let now = network_clock_millis(now);
        if self.config.is_some() && now >= self.lease_expiry_tick {
            self.lose_lease(now);
        }

        if self.dhcp_state == DhcpState::Bound && now >= self.lease_renewal_tick {
            self.dhcp_state = DhcpState::Renewing;
            self.next_dhcp_tick = now;
        } else if self.dhcp_state == DhcpState::Renewing && now >= self.lease_rebinding_tick {
            self.dhcp_state = DhcpState::Rebinding;
            self.next_dhcp_tick = now;
        }

        if now >= self.next_dhcp_tick {
            match self.dhcp_state {
                DhcpState::Init | DhcpState::Selecting => {
                    let length = self.build_dhcp(DHCP_DISCOVER, None, None, None, output)?;
                    self.dhcp_state = DhcpState::Selecting;
                    self.next_dhcp_tick = now.saturating_add(DHCP_RETRY_MILLIS);
                    return Some(length);
                }
                DhcpState::Requesting => {
                    let length = self.build_dhcp(
                        DHCP_REQUEST,
                        Some(self.requested_address),
                        Some(self.dhcp_server),
                        None,
                        output,
                    )?;
                    self.next_dhcp_tick = now.saturating_add(DHCP_RETRY_MILLIS);
                    return Some(length);
                }
                DhcpState::Renewing | DhcpState::Rebinding => {
                    let address = self.config?.address;
                    let length =
                        self.build_dhcp(DHCP_REQUEST, None, None, Some(address), output)?;
                    self.next_dhcp_tick = now.saturating_add(DHCP_RETRY_MILLIS);
                    return Some(length);
                }
                DhcpState::Bound => {}
            }
        }

        let config = self.config?;
        self.management.poll(self.mac, config.address, now, output)
    }

    pub fn handle_frame(&mut self, frame: &[u8], response: &mut [u8], now: usize) -> Option<usize> {
        if frame.len() < ETHERNET_HEADER_LEN || response.len() < ETHERNET_HEADER_LEN {
            return None;
        }

        match read_u16(frame, 12)? {
            ETHERTYPE_ARP => self.handle_arp(frame, response),
            ETHERTYPE_IPV4 => self.handle_ipv4(frame, response, now),
            _ => None,
        }
    }

    fn handle_arp(&self, frame: &[u8], response: &mut [u8]) -> Option<usize> {
        let config = self.config?;
        let packet_len = ETHERNET_HEADER_LEN + ARP_PACKET_LEN;
        if frame.len() < packet_len || response.len() < packet_len {
            return None;
        }
        let arp = &frame[ETHERNET_HEADER_LEN..packet_len];
        if read_u16(arp, 0)? != 1
            || read_u16(arp, 2)? != ETHERTYPE_IPV4
            || arp[4] != 6
            || arp[5] != 4
            || read_u16(arp, 6)? != 1
            || arp[24..28] != config.address
        {
            return None;
        }

        let sender_mac: [u8; 6] = arp[8..14].try_into().ok()?;
        let sender_ip: [u8; 4] = arp[14..18].try_into().ok()?;
        response[..6].copy_from_slice(&sender_mac);
        response[6..12].copy_from_slice(&self.mac);
        write_u16(response, 12, ETHERTYPE_ARP)?;

        let reply = &mut response[ETHERNET_HEADER_LEN..packet_len];
        write_u16(reply, 0, 1)?;
        write_u16(reply, 2, ETHERTYPE_IPV4)?;
        reply[4] = 6;
        reply[5] = 4;
        write_u16(reply, 6, 2)?;
        reply[8..14].copy_from_slice(&self.mac);
        reply[14..18].copy_from_slice(&config.address);
        reply[18..24].copy_from_slice(&sender_mac);
        reply[24..28].copy_from_slice(&sender_ip);
        Some(packet_len)
    }

    fn handle_ipv4(&mut self, frame: &[u8], response: &mut [u8], now: usize) -> Option<usize> {
        if frame.len() < ETHERNET_HEADER_LEN + IPV4_HEADER_LEN {
            return None;
        }
        let ip = &frame[ETHERNET_HEADER_LEN..];
        if ip[0] >> 4 != 4 {
            return None;
        }
        let header_len = (ip[0] as usize & 0x0f) * 4;
        let total_len = read_u16(ip, 2)? as usize;
        if header_len < IPV4_HEADER_LEN
            || total_len < header_len
            || ip.len() < total_len
            || response.len() < ETHERNET_HEADER_LEN + total_len
            || read_u16(ip, 6)? & 0x3fff != 0
            || checksum(&ip[..header_len]) != 0
        {
            return None;
        }

        match ip[9] {
            1 if self
                .config
                .is_some_and(|config| ip[16..20] == config.address) =>
            {
                self.handle_icmp(frame, response, header_len, total_len)
            }
            17 => self.handle_udp(frame, response, header_len, total_len, now),
            6 if self
                .config
                .is_some_and(|config| ip[16..20] == config.address) =>
            {
                let config = self.config.unwrap();
                self.management.handle_ipv4(
                    frame,
                    header_len,
                    total_len,
                    self.mac,
                    config.address,
                    now,
                    response,
                )
            }
            _ => None,
        }
    }

    fn handle_icmp(
        &self,
        frame: &[u8],
        response: &mut [u8],
        ip_header_len: usize,
        ip_total_len: usize,
    ) -> Option<usize> {
        let icmp_offset = ETHERNET_HEADER_LEN + ip_header_len;
        let icmp_len = ip_total_len.checked_sub(ip_header_len)?;
        let frame_len = ETHERNET_HEADER_LEN + ip_total_len;
        if icmp_len < 8
            || frame[icmp_offset] != 8
            || frame[icmp_offset + 1] != 0
            || checksum(&frame[icmp_offset..frame_len]) != 0
        {
            return None;
        }

        response[..frame_len].copy_from_slice(&frame[..frame_len]);
        self.finish_ipv4_reply(response, ip_header_len);
        let icmp = &mut response[icmp_offset..frame_len];
        icmp[0] = 0;
        icmp[2] = 0;
        icmp[3] = 0;
        let value = checksum(icmp);
        write_u16(icmp, 2, value)?;
        Some(frame_len)
    }

    fn handle_udp(
        &mut self,
        frame: &[u8],
        response: &mut [u8],
        ip_header_len: usize,
        ip_total_len: usize,
        now: usize,
    ) -> Option<usize> {
        let udp_offset = ETHERNET_HEADER_LEN + ip_header_len;
        let ip_payload_len = ip_total_len.checked_sub(ip_header_len)?;
        if ip_payload_len < UDP_HEADER_LEN {
            return None;
        }
        let udp = &frame[udp_offset..ETHERNET_HEADER_LEN + ip_total_len];
        let udp_len = read_u16(udp, 4)? as usize;
        if udp_len < UDP_HEADER_LEN || udp_len > ip_payload_len {
            return None;
        }
        let source_port = read_u16(udp, 0)?;
        let destination_port = read_u16(udp, 2)?;
        let source_ip: [u8; 4] = frame[ETHERNET_HEADER_LEN + 12..ETHERNET_HEADER_LEN + 16]
            .try_into()
            .ok()?;
        let destination_ip: [u8; 4] = frame[ETHERNET_HEADER_LEN + 16..ETHERNET_HEADER_LEN + 20]
            .try_into()
            .ok()?;
        let transmitted_checksum = read_u16(udp, 6)?;
        if transmitted_checksum != 0
            && udp_checksum(source_ip, destination_ip, &udp[..udp_len]) != 0
        {
            return None;
        }

        if source_port == DHCP_SERVER_PORT && destination_port == DHCP_CLIENT_PORT {
            return self.handle_dhcp(&udp[UDP_HEADER_LEN..udp_len], source_ip, response, now);
        }

        let config = self.config?;
        if destination_port != CONTROL_PORT
            || frame[ETHERNET_HEADER_LEN + 16..ETHERNET_HEADER_LEN + 20] != config.address
        {
            return None;
        }
        let command = trim_ascii(&udp[UDP_HEADER_LEN..udp_len]);
        let reply = if command.eq_ignore_ascii_case(START_COMMAND) {
            self.start_requested = true;
            START_RESPONSE
        } else {
            COMMAND_ERROR_RESPONSE
        };
        self.build_udp_reply(frame, source_port, source_ip, reply, response)
    }

    fn handle_dhcp(
        &mut self,
        message: &[u8],
        source_ip: [u8; 4],
        response: &mut [u8],
        now: usize,
    ) -> Option<usize> {
        let now = network_clock_millis(now);
        if message.len() < DHCP_FIXED_LEN
            || message[0] != 2
            || message[1] != 1
            || message[2] != 6
            || read_u32(message, 4)? != self.xid
            || message[28..34] != self.mac
            || message[236..240] != DHCP_MAGIC_COOKIE
        {
            return None;
        }
        let options = parse_dhcp_options(&message[DHCP_FIXED_LEN..])?;
        let response_server = options.server.unwrap_or(source_ip);
        match options.message_type? {
            DHCP_OFFER if self.dhcp_state == DhcpState::Selecting => {
                let address: [u8; 4] = message[16..20].try_into().ok()?;
                if address == IPV4_UNSPECIFIED {
                    return None;
                }
                self.requested_address = address;
                self.dhcp_server = response_server;
                let length = self.build_dhcp(
                    DHCP_REQUEST,
                    Some(self.requested_address),
                    Some(self.dhcp_server),
                    None,
                    response,
                )?;
                self.dhcp_state = DhcpState::Requesting;
                self.next_dhcp_tick = now.saturating_add(DHCP_RETRY_MILLIS);
                Some(length)
            }
            DHCP_ACK
                if self.dhcp_state == DhcpState::Requesting
                    && response_server == self.dhcp_server =>
            {
                let offered: [u8; 4] = message[16..20].try_into().ok()?;
                let address = if offered == IPV4_UNSPECIFIED {
                    self.requested_address
                } else {
                    offered
                };
                self.bind_lease(address, response_server, options, now);
                None
            }
            DHCP_ACK
                if (self.dhcp_state == DhcpState::Renewing
                    && response_server == self.dhcp_server)
                    || self.dhcp_state == DhcpState::Rebinding =>
            {
                let current = self.config?;
                let offered: [u8; 4] = message[16..20].try_into().ok()?;
                let address = if offered == IPV4_UNSPECIFIED {
                    current.address
                } else {
                    offered
                };
                // A renewal is for the address already in use. Do not silently
                // replace it and tear down live TCP sessions before expiry.
                if address != current.address {
                    return None;
                }
                self.bind_lease(address, response_server, options, now);
                None
            }
            DHCP_NAK
                if self.dhcp_state == DhcpState::Requesting
                    && response_server == self.dhcp_server =>
            {
                self.lose_lease(now);
                None
            }
            DHCP_NAK
                if (self.dhcp_state == DhcpState::Renewing
                    && response_server == self.dhcp_server)
                    || self.dhcp_state == DhcpState::Rebinding =>
            {
                self.lose_lease(now);
                None
            }
            _ => None,
        }
    }

    fn bind_lease(
        &mut self,
        address: [u8; 4],
        response_server: [u8; 4],
        options: DhcpOptions,
        now: usize,
    ) {
        let previous = self.config;
        let lease_seconds = options
            .lease_seconds
            .or(previous.map(|config| config.lease_seconds))
            .unwrap_or(DEFAULT_LEASE_SECONDS)
            .max(1);
        let new_config = Ipv4Config {
            address,
            subnet_mask: options
                .subnet_mask
                .or(previous.map(|config| config.subnet_mask))
                .unwrap_or(IPV4_UNSPECIFIED),
            router: options
                .router
                .or(previous.map(|config| config.router))
                .unwrap_or(IPV4_UNSPECIFIED),
            dhcp_server: response_server,
            lease_seconds,
        };
        if previous.is_some_and(|config| config.address != new_config.address) {
            self.management.reset_connections();
        }
        let (renewal, rebinding, expiry) = lease_deadlines(
            now,
            lease_seconds,
            options.renewal_seconds,
            options.rebinding_seconds,
        );
        self.config = Some(new_config);
        self.dhcp_server = response_server;
        self.dhcp_state = DhcpState::Bound;
        self.lease_renewal_tick = renewal;
        self.lease_rebinding_tick = rebinding;
        self.lease_expiry_tick = expiry;
        self.next_dhcp_tick = renewal;
    }

    fn lose_lease(&mut self, now: usize) {
        self.config = None;
        self.dhcp_state = DhcpState::Init;
        self.next_dhcp_tick = now;
        self.start_requested = false;
        self.management.reset_connections();
    }

    fn build_dhcp(
        &self,
        message_type: u8,
        requested_address: Option<[u8; 4]>,
        server: Option<[u8; 4]>,
        client_address: Option<[u8; 4]>,
        output: &mut [u8],
    ) -> Option<usize> {
        let frame_len =
            ETHERNET_HEADER_LEN + IPV4_HEADER_LEN + UDP_HEADER_LEN + DHCP_MIN_MESSAGE_LEN;
        if output.len() < frame_len {
            return None;
        }
        output[..frame_len].fill(0);
        output[..6].copy_from_slice(&[0xff; 6]);
        output[6..12].copy_from_slice(&self.mac);
        write_u16(output, 12, ETHERTYPE_IPV4)?;

        let ip = &mut output[ETHERNET_HEADER_LEN..frame_len];
        ip[0] = 0x45;
        write_u16(
            ip,
            2,
            (IPV4_HEADER_LEN + UDP_HEADER_LEN + DHCP_MIN_MESSAGE_LEN) as u16,
        )?;
        ip[8] = 64;
        ip[9] = 17;
        let source_address = client_address.unwrap_or(IPV4_UNSPECIFIED);
        ip[12..16].copy_from_slice(&source_address);
        ip[16..20].copy_from_slice(&IPV4_BROADCAST);
        let ip_checksum = checksum(&ip[..IPV4_HEADER_LEN]);
        write_u16(ip, 10, ip_checksum)?;

        let udp = &mut ip[IPV4_HEADER_LEN..];
        write_u16(udp, 0, DHCP_CLIENT_PORT)?;
        write_u16(udp, 2, DHCP_SERVER_PORT)?;
        write_u16(udp, 4, (UDP_HEADER_LEN + DHCP_MIN_MESSAGE_LEN) as u16)?;
        let dhcp = &mut udp[UDP_HEADER_LEN..UDP_HEADER_LEN + DHCP_MIN_MESSAGE_LEN];
        dhcp[0] = 1;
        dhcp[1] = 1;
        dhcp[2] = 6;
        write_u32(dhcp, 4, self.xid)?;
        write_u16(dhcp, 10, 0x8000)?;
        dhcp[12..16].copy_from_slice(&source_address);
        dhcp[28..34].copy_from_slice(&self.mac);
        dhcp[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);

        let mut option = DHCP_FIXED_LEN;
        put_option(dhcp, &mut option, 53, &[message_type])?;
        let mut client_id = [0u8; 7];
        client_id[0] = 1;
        client_id[1..].copy_from_slice(&self.mac);
        put_option(dhcp, &mut option, 61, &client_id)?;
        if let Some(address) = requested_address {
            put_option(dhcp, &mut option, 50, &address)?;
        }
        if let Some(server) = server {
            put_option(dhcp, &mut option, 54, &server)?;
        }
        put_option(dhcp, &mut option, 55, &[1, 3, 6, 51, 54, 58, 59])?;
        *dhcp.get_mut(option)? = 255;
        let checksum = udp_checksum(source_address, IPV4_BROADCAST, udp);
        write_u16(udp, 6, nonzero_udp_checksum(checksum))?;
        Some(frame_len)
    }

    fn build_udp_reply(
        &self,
        request: &[u8],
        peer_port: u16,
        peer_ip: [u8; 4],
        payload: &[u8],
        output: &mut [u8],
    ) -> Option<usize> {
        let config = self.config?;
        let frame_len = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN + UDP_HEADER_LEN + payload.len();
        if request.len() < ETHERNET_HEADER_LEN || output.len() < frame_len {
            return None;
        }
        output[..frame_len].fill(0);
        output[..6].copy_from_slice(&request[6..12]);
        output[6..12].copy_from_slice(&self.mac);
        write_u16(output, 12, ETHERTYPE_IPV4)?;
        let ip = &mut output[ETHERNET_HEADER_LEN..];
        ip[0] = 0x45;
        write_u16(
            ip,
            2,
            (IPV4_HEADER_LEN + UDP_HEADER_LEN + payload.len()) as u16,
        )?;
        ip[8] = 64;
        ip[9] = 17;
        ip[12..16].copy_from_slice(&config.address);
        ip[16..20].copy_from_slice(&peer_ip);
        let ip_checksum = checksum(&ip[..IPV4_HEADER_LEN]);
        write_u16(ip, 10, ip_checksum)?;
        let udp = &mut ip[IPV4_HEADER_LEN..];
        write_u16(udp, 0, CONTROL_PORT)?;
        write_u16(udp, 2, peer_port)?;
        write_u16(udp, 4, (UDP_HEADER_LEN + payload.len()) as u16)?;
        udp[UDP_HEADER_LEN..UDP_HEADER_LEN + payload.len()].copy_from_slice(payload);
        let checksum = udp_checksum(config.address, peer_ip, udp);
        write_u16(udp, 6, nonzero_udp_checksum(checksum))?;
        Some(frame_len)
    }

    fn finish_ipv4_reply(&self, response: &mut [u8], ip_header_len: usize) {
        let config = self.config.unwrap();
        let peer_mac: [u8; 6] = response[6..12].try_into().unwrap();
        response[..6].copy_from_slice(&peer_mac);
        response[6..12].copy_from_slice(&self.mac);
        let ip = &mut response[ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + ip_header_len];
        let peer_ip: [u8; 4] = ip[12..16].try_into().unwrap();
        ip[12..16].copy_from_slice(&config.address);
        ip[16..20].copy_from_slice(&peer_ip);
        ip[8] = 64;
        ip[10] = 0;
        ip[11] = 0;
        let value = checksum(ip);
        ip[10..12].copy_from_slice(&value.to_be_bytes());
    }
}

fn parse_dhcp_options(bytes: &[u8]) -> Option<DhcpOptions> {
    let mut result = DhcpOptions::default();
    let mut index = 0;
    while index < bytes.len() {
        let code = bytes[index];
        index += 1;
        if code == 0 {
            continue;
        }
        if code == 255 {
            return Some(result);
        }
        let length = *bytes.get(index)? as usize;
        index += 1;
        let value = bytes.get(index..index.checked_add(length)?)?;
        index += length;
        match (code, length) {
            (53, 1) => result.message_type = Some(value[0]),
            (54, 4) => result.server = Some(value.try_into().ok()?),
            (1, 4) => result.subnet_mask = Some(value.try_into().ok()?),
            (3, length) if length >= 4 => result.router = Some(value[..4].try_into().ok()?),
            (51, 4) => result.lease_seconds = Some(u32::from_be_bytes(value.try_into().ok()?)),
            (58, 4) => result.renewal_seconds = Some(u32::from_be_bytes(value.try_into().ok()?)),
            (59, 4) => result.rebinding_seconds = Some(u32::from_be_bytes(value.try_into().ok()?)),
            _ => {}
        }
    }
    Some(result)
}

fn lease_deadlines(
    now: usize,
    lease_seconds: u32,
    renewal_seconds: Option<u32>,
    rebinding_seconds: Option<u32>,
) -> (usize, usize, usize) {
    let lease_millis = (lease_seconds.max(1) as usize).saturating_mul(MILLIS_PER_SECOND);
    let default_renewal = lease_millis / 2;
    let default_rebinding = lease_millis.saturating_mul(7) / 8;
    let requested_renewal = renewal_seconds
        .map(|seconds| (seconds as usize).saturating_mul(MILLIS_PER_SECOND))
        .unwrap_or(default_renewal);
    let requested_rebinding = rebinding_seconds
        .map(|seconds| (seconds as usize).saturating_mul(MILLIS_PER_SECOND))
        .unwrap_or(default_rebinding);
    let (renewal, rebinding) = if requested_renewal > 0
        && requested_renewal < requested_rebinding
        && requested_rebinding < lease_millis
    {
        (requested_renewal, requested_rebinding)
    } else {
        (default_renewal, default_rebinding)
    };
    (
        now.saturating_add(renewal),
        now.saturating_add(rebinding),
        now.saturating_add(lease_millis),
    )
}

fn put_option(bytes: &mut [u8], index: &mut usize, code: u8, value: &[u8]) -> Option<()> {
    if value.len() > u8::MAX as usize {
        return None;
    }
    *bytes.get_mut(*index)? = code;
    *bytes.get_mut(*index + 1)? = value.len() as u8;
    bytes
        .get_mut(*index + 2..*index + 2 + value.len())?
        .copy_from_slice(value);
    *index += 2 + value.len();
    Some(())
}

#[cfg(not(test))]
fn network_clock_millis(fallback: usize) -> usize {
    let Some(&tsc_khz) = crate::interrupt::apic::GUEST_TSC_KHZ.get() else {
        return fallback;
    };
    if tsc_khz == 0 {
        return fallback;
    }
    (unsafe { core::arch::x86_64::_rdtsc() } / tsc_khz) as usize
}

#[cfg(test)]
fn network_clock_millis(fallback: usize) -> usize {
    fallback
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
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

fn udp_checksum(source: [u8; 4], destination: [u8; 4], udp: &[u8]) -> u16 {
    let mut sum = 0u32;
    sum = add_bytes(sum, &source);
    sum = add_bytes(sum, &destination);
    sum = sum.wrapping_add(17);
    sum = sum.wrapping_add(udp.len() as u32);
    finish_sum(add_bytes(sum, udp))
}

fn nonzero_udp_checksum(checksum: u16) -> u16 {
    if checksum == 0 { u16::MAX } else { checksum }
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

    const MAC: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
    const LEASED_IP: [u8; 4] = [10, 0, 2, 15];
    const SERVER_IP: [u8; 4] = [10, 0, 2, 2];

    fn dhcp_reply_from(
        stack: &NetworkStack,
        message_type: u8,
        address: [u8; 4],
        server_ip: [u8; 4],
    ) -> [u8; 342] {
        dhcp_reply_with_lease(stack, message_type, address, server_ip, 3600, None, None)
    }

    fn dhcp_reply_with_lease(
        stack: &NetworkStack,
        message_type: u8,
        address: [u8; 4],
        server_ip: [u8; 4],
        lease_seconds: u32,
        renewal_seconds: Option<u32>,
        rebinding_seconds: Option<u32>,
    ) -> [u8; 342] {
        let mut frame = [0u8; 342];
        frame[..6].copy_from_slice(&MAC);
        frame[6..12].copy_from_slice(&[0x52, 0x55, 10, 0, 2, 2]);
        write_u16(&mut frame, 12, ETHERTYPE_IPV4).unwrap();
        let ip = &mut frame[14..];
        ip[0] = 0x45;
        write_u16(ip, 2, 328).unwrap();
        ip[8] = 64;
        ip[9] = 17;
        ip[12..16].copy_from_slice(&server_ip);
        ip[16..20].copy_from_slice(&IPV4_BROADCAST);
        let value = checksum(&ip[..20]);
        write_u16(ip, 10, value).unwrap();
        let udp = &mut ip[20..];
        write_u16(udp, 0, DHCP_SERVER_PORT).unwrap();
        write_u16(udp, 2, DHCP_CLIENT_PORT).unwrap();
        write_u16(udp, 4, 308).unwrap();
        let dhcp = &mut udp[8..];
        dhcp[0] = 2;
        dhcp[1] = 1;
        dhcp[2] = 6;
        write_u32(dhcp, 4, stack.xid).unwrap();
        dhcp[16..20].copy_from_slice(&address);
        dhcp[28..34].copy_from_slice(&MAC);
        dhcp[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);
        let mut option = 240;
        put_option(dhcp, &mut option, 53, &[message_type]).unwrap();
        put_option(dhcp, &mut option, 54, &server_ip).unwrap();
        put_option(dhcp, &mut option, 1, &[255, 255, 255, 0]).unwrap();
        put_option(dhcp, &mut option, 3, &server_ip).unwrap();
        put_option(dhcp, &mut option, 51, &lease_seconds.to_be_bytes()).unwrap();
        if let Some(seconds) = renewal_seconds {
            put_option(dhcp, &mut option, 58, &seconds.to_be_bytes()).unwrap();
        }
        if let Some(seconds) = rebinding_seconds {
            put_option(dhcp, &mut option, 59, &seconds.to_be_bytes()).unwrap();
        }
        dhcp[option] = 255;
        frame
    }

    fn dhcp_reply(stack: &NetworkStack, message_type: u8, address: [u8; 4]) -> [u8; 342] {
        dhcp_reply_from(stack, message_type, address, SERVER_IP)
    }

    fn acquire_lease(stack: &mut NetworkStack, output: &mut [u8]) {
        stack.poll(0, output).unwrap();
        let offer = dhcp_reply(stack, DHCP_OFFER, LEASED_IP);
        stack.handle_frame(&offer, output, 1).unwrap();
        let ack = dhcp_reply(stack, DHCP_ACK, LEASED_IP);
        assert_eq!(stack.handle_frame(&ack, output, 2), None);
    }

    fn acquire_short_lease(stack: &mut NetworkStack, output: &mut [u8]) {
        stack.poll(0, output).unwrap();
        let offer = dhcp_reply(stack, DHCP_OFFER, LEASED_IP);
        stack.handle_frame(&offer, output, 1).unwrap();
        let ack = dhcp_reply_with_lease(stack, DHCP_ACK, LEASED_IP, SERVER_IP, 8, None, None);
        assert_eq!(stack.handle_frame(&ack, output, 2), None);
    }

    fn has_dhcp_option(mut bytes: &[u8], expected: u8) -> bool {
        while let Some((&code, rest)) = bytes.split_first() {
            bytes = rest;
            match code {
                0 => continue,
                255 => return false,
                _ => {
                    let Some((&length, rest)) = bytes.split_first() else {
                        return false;
                    };
                    let length = length as usize;
                    if rest.len() < length {
                        return false;
                    }
                    if code == expected {
                        return true;
                    }
                    bytes = &rest[length..];
                }
            }
        }
        false
    }

    fn control_request(command: &[u8]) -> [u8; 64] {
        let mut frame = [0u8; 64];
        let frame_len = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN + UDP_HEADER_LEN + command.len();
        frame[..6].copy_from_slice(&MAC);
        frame[6..12].copy_from_slice(&[0x52, 0x55, 10, 0, 2, 2]);
        write_u16(&mut frame, 12, ETHERTYPE_IPV4).unwrap();
        let ip = &mut frame[ETHERNET_HEADER_LEN..frame_len];
        ip[0] = 0x45;
        write_u16(
            ip,
            2,
            (IPV4_HEADER_LEN + UDP_HEADER_LEN + command.len()) as u16,
        )
        .unwrap();
        ip[8] = 64;
        ip[9] = 17;
        ip[12..16].copy_from_slice(&SERVER_IP);
        ip[16..20].copy_from_slice(&LEASED_IP);
        let value = checksum(&ip[..IPV4_HEADER_LEN]);
        write_u16(ip, 10, value).unwrap();
        let udp = &mut ip[IPV4_HEADER_LEN..];
        write_u16(udp, 0, 43123).unwrap();
        write_u16(udp, 2, CONTROL_PORT).unwrap();
        write_u16(udp, 4, (UDP_HEADER_LEN + command.len()) as u16).unwrap();
        udp[UDP_HEADER_LEN..UDP_HEADER_LEN + command.len()].copy_from_slice(command);
        frame
    }

    #[test]
    fn performs_dhcp_discover_request_ack() {
        let mut stack = NetworkStack::new(MAC);
        let mut output = [0u8; 512];
        let discover_len = stack.poll(0, &mut output).unwrap();
        assert_eq!(read_u16(&output[34..], 0), Some(DHCP_CLIENT_PORT));
        assert_ne!(read_u16(&output[34..], 6), Some(0));
        assert_eq!(
            udp_checksum(IPV4_UNSPECIFIED, IPV4_BROADCAST, &output[34..discover_len]),
            0
        );
        assert_eq!(output[42], 1);
        assert_eq!(discover_len, 342);

        let offer = dhcp_reply(&stack, DHCP_OFFER, LEASED_IP);
        let request_len = stack.handle_frame(&offer, &mut output, 1).unwrap();
        assert_eq!(request_len, 342);
        let request_options = parse_dhcp_options(&output[42 + DHCP_FIXED_LEN..]).unwrap();
        assert_eq!(request_options.message_type, Some(DHCP_REQUEST));

        let ack = dhcp_reply(&stack, DHCP_ACK, LEASED_IP);
        assert_eq!(stack.handle_frame(&ack, &mut output, 2), None);
        assert_eq!(
            stack.ipv4_config(),
            Some(Ipv4Config {
                address: LEASED_IP,
                subnet_mask: [255, 255, 255, 0],
                router: SERVER_IP,
                dhcp_server: SERVER_IP,
                lease_seconds: 3600,
            })
        );
    }

    #[test]
    fn ignores_nak_from_unselected_dhcp_server() {
        const FOREIGN_SERVER: [u8; 4] = [10, 0, 3, 1];

        let mut stack = NetworkStack::new(MAC);
        let mut output = [0u8; 512];
        stack.poll(0, &mut output).unwrap();
        let offer = dhcp_reply(&stack, DHCP_OFFER, LEASED_IP);
        stack.handle_frame(&offer, &mut output, 1).unwrap();

        let foreign_nak = dhcp_reply_from(&stack, DHCP_NAK, IPV4_UNSPECIFIED, FOREIGN_SERVER);
        assert_eq!(stack.handle_frame(&foreign_nak, &mut output, 2), None);
        assert_eq!(stack.dhcp_state, DhcpState::Requesting);
        assert_eq!(stack.dhcp_server, SERVER_IP);

        let ack = dhcp_reply(&stack, DHCP_ACK, LEASED_IP);
        assert_eq!(stack.handle_frame(&ack, &mut output, 3), None);
        assert_eq!(stack.ipv4_config().unwrap().address, LEASED_IP);
    }

    #[test]
    fn renews_lease_without_replacing_the_address() {
        let mut stack = NetworkStack::new(MAC);
        let mut output = [0u8; 512];
        acquire_short_lease(&mut stack, &mut output);

        assert_eq!(stack.lease_renewal_tick, 4_002);
        assert_eq!(stack.lease_rebinding_tick, 7_002);
        assert_eq!(stack.lease_expiry_tick, 8_002);
        let renewal_len = stack.poll(4_002, &mut output).unwrap();
        assert_eq!(stack.dhcp_state, DhcpState::Renewing);
        assert_eq!(stack.ipv4_config().unwrap().address, LEASED_IP);
        assert_eq!(&output[26..30], &LEASED_IP);
        assert_eq!(&output[54..58], &LEASED_IP);
        assert_eq!(
            udp_checksum(LEASED_IP, IPV4_BROADCAST, &output[34..renewal_len]),
            0
        );
        let options = &output[42 + DHCP_FIXED_LEN..renewal_len];
        assert!(!has_dhcp_option(options, 50));
        assert!(!has_dhcp_option(options, 54));

        let changed_address = [10, 0, 2, 99];
        let invalid_ack =
            dhcp_reply_with_lease(&stack, DHCP_ACK, changed_address, SERVER_IP, 8, None, None);
        assert_eq!(stack.handle_frame(&invalid_ack, &mut output, 4_003), None);
        assert_eq!(stack.dhcp_state, DhcpState::Renewing);
        assert_eq!(stack.ipv4_config().unwrap().address, LEASED_IP);

        let ack =
            dhcp_reply_with_lease(&stack, DHCP_ACK, IPV4_UNSPECIFIED, SERVER_IP, 8, None, None);
        assert_eq!(stack.handle_frame(&ack, &mut output, 4_004), None);
        assert_eq!(stack.dhcp_state, DhcpState::Bound);
        assert_eq!(stack.ipv4_config().unwrap().address, LEASED_IP);
        assert_eq!(stack.lease_expiry_tick, 12_004);
    }

    #[test]
    fn keeps_lease_through_rebinding_and_drops_it_only_at_expiry() {
        let mut stack = NetworkStack::new(MAC);
        let mut output = [0u8; 512];
        acquire_short_lease(&mut stack, &mut output);

        stack.poll(4_002, &mut output).unwrap();
        assert_eq!(stack.dhcp_state, DhcpState::Renewing);
        assert!(stack.ipv4_config().is_some());
        stack.poll(7_002, &mut output).unwrap();
        assert_eq!(stack.dhcp_state, DhcpState::Rebinding);
        assert!(stack.ipv4_config().is_some());
        assert_eq!(stack.poll(8_001, &mut output), None);
        assert!(stack.ipv4_config().is_some());

        let discover_len = stack.poll(8_002, &mut output).unwrap();
        assert_eq!(stack.dhcp_state, DhcpState::Selecting);
        assert_eq!(stack.ipv4_config(), None);
        let options = parse_dhcp_options(&output[42 + DHCP_FIXED_LEN..discover_len]).unwrap();
        assert_eq!(options.message_type, Some(DHCP_DISCOVER));
    }

    #[test]
    fn honors_server_supplied_renewal_and_rebinding_times() {
        let mut stack = NetworkStack::new(MAC);
        let mut output = [0u8; 512];
        stack.poll(0, &mut output).unwrap();
        let offer = dhcp_reply(&stack, DHCP_OFFER, LEASED_IP);
        stack.handle_frame(&offer, &mut output, 1).unwrap();
        let ack =
            dhcp_reply_with_lease(&stack, DHCP_ACK, LEASED_IP, SERVER_IP, 10, Some(2), Some(8));
        stack.handle_frame(&ack, &mut output, 2);

        assert_eq!(stack.lease_renewal_tick, 2_002);
        assert_eq!(stack.lease_rebinding_tick, 8_002);
        assert_eq!(stack.lease_expiry_tick, 10_002);
    }

    #[test]
    fn start_command_is_ignored_before_dhcp() {
        let mut stack = NetworkStack::new(MAC);
        let request = control_request(b"start\n");
        let mut output = [0u8; 512];
        assert_eq!(stack.handle_frame(&request, &mut output, 0), None);
        assert!(!stack.take_start_request());
    }

    #[test]
    fn start_command_opens_gate_after_dhcp() {
        let mut stack = NetworkStack::new(MAC);
        let mut output = [0u8; 512];
        acquire_lease(&mut stack, &mut output);
        let request = control_request(b" start\r\n");
        let length = stack.handle_frame(&request, &mut output, 3).unwrap();
        assert_eq!(&output[42..length], START_RESPONSE);
        assert!(stack.take_start_request());
        assert!(!stack.take_start_request());
    }

    #[test]
    fn trims_control_command() {
        assert_eq!(trim_ascii(b"  start\r\n"), b"start");
    }
}
