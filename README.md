![check](https://git.mii.dev/mii/nel_os_uefi/actions/workflows/check.yaml/badge.svg?branch=main)

## Hypervisor-only network

The outer kernel owns a transitional virtio-net PCI device; it is not mapped
or enumerated in the Linux guest. The hypervisor obtains its IPv4 address,
subnet mask, router, and lease time using DHCP, and answers ARP and ICMP echo.

The Linux VM remains uncreated until a local-serial or network management
command starts it. After DHCP succeeds, the hypervisor exposes a TCP management
shell on port `5555`. The legacy UDP `start` command on the same numeric port is
retained for simple automation.

### Default port forwarding

The default `cargo r -r` command uses QEMU user-mode networking. QEMU assigns
`10.0.2.15` to the hypervisor over DHCP and forwards both TCP and UDP host port
`5555` to the hypervisor management port:

```sh
cd nel_os_bootloader
cargo r -r
```

From another terminal, open the interactive management shell:

```sh
nc 127.0.0.1 5555
```

The shell supports:

```text
vm start               create and run the VM, or resume a stopped VM
vm start --attach      start/resume the VM and attach its serial (`-a` also works)
vm stop                stop VCPU execution while retaining guest memory
vm reset               reset in place and start the VM
vm status              show the current VM state and guest memory size (`vm info` also works)
serial attach          attach the connection to the guest COM1 byte stream
serial detach          explicitly detach while at the management prompt
info memory            show current host physical-memory use
info runtime           show kernel, CPU, virtualization, uptime, and IPv4 data
info all               show runtime, memory, and VM information
help
exit
```

The same `nel>` management shell is available on the hypervisor's physical
COM1 console as soon as kernel initialization completes. This local console
remains available if the network device fails. `exit` closes a TCP session; on
the non-disconnectable local console it simply returns another prompt. Pressing
Enter on an empty line also redraws `nel>`.

The embedded TCP server accepts one management connection at a time. Close or
`exit` the current session before connecting another client.

While attached to the VM serial port, press `Ctrl-]` to detach and return to
the `nel>` prompt. Guest COM1 output is hidden by default and is sent only to
management consoles that explicitly ran `serial attach` or
`vm start --attach`; in particular, a TCP attachment does not expose guest
output on the physical hypervisor console. Only one console can own the guest
serial stream at a time; another console's attach request is rejected until the
owner detaches. `info runtime` reports any guest-output bytes dropped because a
slow TCP client exhausted the bounded serial buffers.

The UDP start command remains available:

```sh
printf 'start\n' | nc -u -w 2 127.0.0.1 5555
```

`NEL_OS_NET_BIND_ADDR` and `NEL_OS_NET_HOST_PORT` change the host-side bind
address and TCP/UDP port. The default loopback binding does not expose the
management shell to other machines; use `0.0.0.0` only on a trusted network.

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
hypervisor obtains its address directly from the LAN DHCP server. Connect to
the address printed in the serial console's `DHCP lease` message:

```sh
nc <hypervisor-dhcp-address> 5555
```

Or use the compatibility UDP start command:

```sh
printf 'start\n' | nc -u -w 2 <hypervisor-dhcp-address> 5555
```

Devices on the same LAN can connect directly in bridge mode; there is no host
port forwarding in that mode. The management shell is intentionally
unauthenticated, so only expose bridge mode or a non-loopback bind address on a
trusted management network.

`NEL_OS_NET_BRIDGE` and `NEL_OS_NET_TAP` can override the default interface
names. `NEL_OS_NET_MAC` overrides the default `52:54:00:12:34:56` virtio-net
address; give every simultaneously running instance on the same LAN a unique
locally administered MAC address.

To remove a tap created by the setup script without changing the physical
bridge:

```sh
./setup-br0.sh down
```
