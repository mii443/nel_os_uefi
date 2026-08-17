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
const DHCP_RETRY_TICKS: usize = 4 * 250;
const DEFAULT_LEASE_SECONDS: u32 = 3600;
const TICKS_PER_SECOND: usize = 250;

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
}

#[derive(Default)]
struct DhcpOptions {
    message_type: Option<u8>,
    server: Option<[u8; 4]>,
    subnet_mask: Option<[u8; 4]>,
    router: Option<[u8; 4]>,
    lease_seconds: Option<u32>,
}

pub struct NetworkStack {
    mac: [u8; 6],
    xid: u32,
    dhcp_state: DhcpState,
    requested_address: [u8; 4],
    dhcp_server: [u8; 4],
    next_dhcp_tick: usize,
    lease_expiry_tick: usize,
    config: Option<Ipv4Config>,
    start_requested: bool,
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
            lease_expiry_tick: 0,
            config: None,
            start_requested: false,
        }
    }

    pub fn ipv4_config(&self) -> Option<Ipv4Config> {
        self.config
    }

    pub fn take_start_request(&mut self) -> bool {
        core::mem::take(&mut self.start_requested)
    }

    pub fn poll(&mut self, now: usize, output: &mut [u8]) -> Option<usize> {
        if self.dhcp_state == DhcpState::Bound && now >= self.lease_expiry_tick {
            self.config = None;
            self.dhcp_state = DhcpState::Init;
            self.next_dhcp_tick = now;
        }

        if now < self.next_dhcp_tick {
            return None;
        }

        match self.dhcp_state {
            DhcpState::Init | DhcpState::Selecting => {
                let length = self.build_dhcp(DHCP_DISCOVER, None, None, output)?;
                self.dhcp_state = DhcpState::Selecting;
                self.next_dhcp_tick = now.saturating_add(DHCP_RETRY_TICKS);
                Some(length)
            }
            DhcpState::Requesting => {
                let length = self.build_dhcp(
                    DHCP_REQUEST,
                    Some(self.requested_address),
                    Some(self.dhcp_server),
                    output,
                )?;
                self.next_dhcp_tick = now.saturating_add(DHCP_RETRY_TICKS);
                Some(length)
            }
            DhcpState::Bound => None,
        }
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
        match options.message_type? {
            DHCP_OFFER if self.dhcp_state == DhcpState::Selecting => {
                let address: [u8; 4] = message[16..20].try_into().ok()?;
                if address == IPV4_UNSPECIFIED {
                    return None;
                }
                self.requested_address = address;
                self.dhcp_server = options.server.unwrap_or(source_ip);
                let length = self.build_dhcp(
                    DHCP_REQUEST,
                    Some(self.requested_address),
                    Some(self.dhcp_server),
                    response,
                )?;
                self.dhcp_state = DhcpState::Requesting;
                self.next_dhcp_tick = now.saturating_add(DHCP_RETRY_TICKS);
                Some(length)
            }
            DHCP_ACK if self.dhcp_state == DhcpState::Requesting => {
                let offered: [u8; 4] = message[16..20].try_into().ok()?;
                let address = if offered == IPV4_UNSPECIFIED {
                    self.requested_address
                } else {
                    offered
                };
                let lease_seconds = options.lease_seconds.unwrap_or(DEFAULT_LEASE_SECONDS);
                let lease_ticks = (lease_seconds as usize).saturating_mul(TICKS_PER_SECOND);
                self.config = Some(Ipv4Config {
                    address,
                    subnet_mask: options.subnet_mask.unwrap_or(IPV4_UNSPECIFIED),
                    router: options.router.unwrap_or(IPV4_UNSPECIFIED),
                    dhcp_server: options.server.unwrap_or(self.dhcp_server),
                    lease_seconds,
                });
                self.dhcp_state = DhcpState::Bound;
                self.lease_expiry_tick = now.saturating_add(lease_ticks);
                self.next_dhcp_tick = usize::MAX;
                None
            }
            DHCP_NAK => {
                self.config = None;
                self.dhcp_state = DhcpState::Init;
                self.next_dhcp_tick = now;
                None
            }
            _ => None,
        }
    }

    fn build_dhcp(
        &self,
        message_type: u8,
        requested_address: Option<[u8; 4]>,
        server: Option<[u8; 4]>,
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
        ip[12..16].copy_from_slice(&IPV4_UNSPECIFIED);
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
        put_option(dhcp, &mut option, 55, &[1, 3, 6, 51, 54])?;
        *dhcp.get_mut(option)? = 255;
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
            _ => {}
        }
    }
    Some(result)
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
    let mut sum = 0u32;
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(&last) = chunks.remainder().first() {
        sum += (last as u32) << 8;
    }
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

    fn dhcp_reply(stack: &NetworkStack, message_type: u8, address: [u8; 4]) -> [u8; 342] {
        let mut frame = [0u8; 342];
        frame[..6].copy_from_slice(&MAC);
        frame[6..12].copy_from_slice(&[0x52, 0x55, 10, 0, 2, 2]);
        write_u16(&mut frame, 12, ETHERTYPE_IPV4).unwrap();
        let ip = &mut frame[14..];
        ip[0] = 0x45;
        write_u16(ip, 2, 328).unwrap();
        ip[8] = 64;
        ip[9] = 17;
        ip[12..16].copy_from_slice(&SERVER_IP);
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
        put_option(dhcp, &mut option, 54, &SERVER_IP).unwrap();
        put_option(dhcp, &mut option, 1, &[255, 255, 255, 0]).unwrap();
        put_option(dhcp, &mut option, 3, &SERVER_IP).unwrap();
        put_option(dhcp, &mut option, 51, &3600u32.to_be_bytes()).unwrap();
        dhcp[option] = 255;
        frame
    }

    fn acquire_lease(stack: &mut NetworkStack, output: &mut [u8]) {
        stack.poll(0, output).unwrap();
        let offer = dhcp_reply(stack, DHCP_OFFER, LEASED_IP);
        stack.handle_frame(&offer, output, 1).unwrap();
        let ack = dhcp_reply(stack, DHCP_ACK, LEASED_IP);
        assert_eq!(stack.handle_frame(&ack, output, 2), None);
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
