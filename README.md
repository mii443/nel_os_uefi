![check](https://git.mii.dev/mii/nel_os_uefi/actions/workflows/check.yaml/badge.svg?branch=main)

## Hypervisor-only network

The outer kernel owns a transitional virtio-net PCI device; it is not mapped
or enumerated in the Linux guest. The hypervisor obtains its IPv4 address,
subnet mask, router, and lease time using DHCP, and answers ARP and ICMP echo.

The Linux VM is not created until DHCP succeeds and the hypervisor receives the
UDP command `start` on port `5555`.

### Default port forwarding

The default `cargo r -r` command uses QEMU user-mode networking. QEMU assigns
`10.0.2.15` to the hypervisor over DHCP and forwards host UDP port `5555` to
the hypervisor control port:

```sh
cd nel_os_bootloader
cargo r -r
```

From another terminal, start the Linux VM through the forwarded port:

```sh
printf 'start\n' | nc -u -w 2 127.0.0.1 5555
```

`NEL_OS_NET_BIND_ADDR` and `NEL_OS_NET_HOST_PORT` change the host-side bind
address and UDP port. The default loopback binding does not expose the control
port to other machines; use `0.0.0.0` only on a trusted network.

### Physical bridge mode

Set `NEL_OS_NET_MODE=bridge` to connect the hypervisor directly to a physical
LAN. This is an explicit mode; there is no automatic fallback between bridge
and user-mode networking. QEMU uses `tap-nel0` attached to a Linux bridge named
`br0`, without invoking QEMU's bridge helper. The bridge must already be
connected to a physical LAN that provides DHCP:

```text
virtio-net -> tap-nel0 -> br0 -> physical NIC -> LAN DHCP server
```

Configure the host's addresses and default route on `br0`, not on its member
physical interface. For example, a netplan configuration using `eno1` is:

```yaml
network:
  version: 2
  ethernets:
    eno1:
      dhcp4: false
  bridges:
    br0:
      interfaces: [eno1]
      dhcp4: true
      parameters:
        stp: false
        forward-delay: 0
```

Apply the equivalent bridge configuration with NetworkManager or
systemd-networkd on distributions that do not use netplan. Moving a host's
management interface into a bridge can interrupt the connection, so perform
that operation from a local console or another management interface.

After the physical bridge is ready, create the QEMU tap and start bridge mode:

```sh
cd nel_os_bootloader
./setup-br0.sh up
NEL_OS_NET_MODE=bridge cargo r -r
```

The setup script only creates `tap-nel0`; it never creates `br0`, changes a
physical interface, starts a DHCP server, or installs firewall/NAT rules. The
hypervisor obtains its address directly from the LAN DHCP server. Send `start`
to the address printed in the serial console's `DHCP lease` message:

```sh
printf 'start\n' | nc -u -w 2 <hypervisor-dhcp-address> 5555
```

Devices on the same LAN can send the command directly in bridge mode; there is
no host port forwarding in that mode. The control command is intentionally
minimal and unauthenticated, so only use it on a trusted network.

`NEL_OS_NET_BRIDGE` and `NEL_OS_NET_TAP` can override the default interface
names. `NEL_OS_NET_MAC` overrides the default `52:54:00:12:34:56` virtio-net
address; give every simultaneously running instance on the same LAN a unique
locally administered MAC address.

To remove a tap created by the setup script without changing the physical
bridge:

```sh
./setup-br0.sh down
```
