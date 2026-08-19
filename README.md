![check](https://git.mii.dev/mii/nel_os_uefi/actions/workflows/check.yaml/badge.svg?branch=main)

## Management and passthrough networks

QEMU gives the hypervisor two virtio-net PCI functions. The outer kernel owns
the first transitional device and obtains its management IPv4 configuration by
DHCP. VM 0 exclusively owns the second, modern-only device through PCI
passthrough. The hypervisor exposes that device's PCI configuration and MMIO
BARs, while the QEMU Intel IOMMU translates guest DMA without exposing
hypervisor memory.

At boot the hypervisor creates and starts VM 0 with 256 MiB of RAM and one
vCPU. Additional VMs are created dynamically with an explicit RAM size. There
is no fixed VM-slot count; creation is limited by available host memory and
hardware virtualization resources. VCPUs are scheduled round-robin on QEMU's
one hypervisor CPU. After management DHCP succeeds, the hypervisor exposes a
TCP management shell on port `5555`. The legacy UDP `start` command remains
available and is idempotent for the already-running VM 0.

### Shared checkout and local builds

The canonical checkout is `/home/mii/nas-work/nel_os_uefi`, which is available
on both development hosts. Run the root wrapper from either machine:

```sh
cd /home/mii/nas-work/nel_os_uefi
./run.sh
```

The wrapper keeps Cargo output in `~/.cache/nel_os_uefi/target` and QEMU's
generated ISO and writable UEFI variables under the host-local runtime
directory (`/tmp/nel_os_uefi-$UID` by default). The shared NFS checkout is
therefore not used for host-specific build or runtime state, and the AMD and
Intel hosts can build and run independently. `NEL_OS_LOCAL_CACHE_DIR`,
`NEL_OS_CARGO_TARGET_DIR`, and `NEL_OS_RUNTIME_BASE` override these locations.

### Host virtio block device

QEMU also gives the outer hypervisor one transitional virtio-blk device. The
block device is retained by the hypervisor and is never passed through to VM
0. Instead, the outer kernel implements a separate legacy virtio-blk PCI
device for the VM and services its virtqueue from the host-owned backing
device. `info runtime` and `info all` report the backing device capacity.

VM 0 starts at the architectural x86 reset vector in its own OVMF firmware.
That firmware discovers the emulated virtio-blk device, reads a normal GPT
disk and EFI System Partition, starts systemd-boot, and finally launches the
Linux EFI stub. The hypervisor therefore does not parse or directly load a
Linux kernel from the disk image.

By default, `./run.sh` regenerates a 64 MiB GPT disk named
`host-block.img` in the host-local runtime directory from
`nel_os_bootloader/bzImage` and `rootfs-n.cpio.gz`. Set
`NEL_OS_BLOCK_SIZE_MIB` to choose its size. To build and attach another boot
bundle explicitly:

```sh
./nel_os_bootloader/create-linux-disk.sh /tmp/linux.img 64 \
    ./nel_os_bootloader/bzImage ./nel_os_bootloader/rootfs-n.cpio.gz
NEL_OS_BLOCK_IMAGE=/tmp/linux.img ./run.sh
```

An explicit image must already exist and be UEFI bootable; it is attached
without being modified. This allows another Linux distribution or another
UEFI-capable operating system image to use the same virtual disk interface.
`NEL_OS_BLOCK_SIZE_MIB` applies only when the default image is generated.

On a new Ubuntu host, install the host tools and rustup once:

```sh
sudo apt-get install qemu-system-x86 ovmf xorriso mtools gdisk systemd-boot-efi curl ca-certificates
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
sudo usermod -aG kvm "$USER"
```

Log out and back in after the group change. The pinned toolchain and its Rust
components are then installed automatically by the first `./run.sh` invocation.

### Default port forwarding

The default `./run.sh` command gives the hypervisor 1 GiB of RAM and uses two
QEMU user-mode networks. QEMU assigns `10.0.2.15` to the hypervisor and can
assign `10.0.3.15` to VM 0's passed-through NIC. Both TCP and UDP host port
`5555` are forwarded to the hypervisor management port:

```sh
cd /home/mii/nas-work/nel_os_uefi
./run.sh
```

From another terminal, open the interactive management shell:

```sh
nc 127.0.0.1 5555
```

The shell supports:

```text
vm list                show all created VMs
vm create [ID] MEMORY  create a VM; the runtime limit follows available host RAM
vm start [ID]          start a created VM, or resume it when stopped
vm start [ID] -a       start/resume and attach its serial (`--attach` also works)
vm stop [ID]           stop one VCPU while retaining its guest memory
vm reset [ID]          reset one VM in place and start it
vm delete [ID]         delete a VM and release its host memory (`remove` also works)
vm status [ID]         show one VM (`vm info` and `info vm` also work)
serial attach [ID]     attach to one VM's COM1 byte stream
serial detach          explicitly detach while at the management prompt
info memory            show current host physical-memory use
info runtime           show kernel, CPU, virtualization, disk, uptime, and IPv4 data
info all               show runtime, memory, and all VM information
help
exit
```

For `vm create`, omitting `ID` selects the lowest unused ID. Because VM 0 is
created during boot, the first manual create normally selects VM 1. Other
commands default to VM 0 when `ID` is omitted. VM creation records the requested
RAM size but does not allocate all of its backing or execute Linux; run
`vm start [ID]` separately. Guest RAM is backed on demand while the VM runs.
Stopping a VM retains its allocation and state while the other running VMs
continue to execute. Deleting a VM stops it, detaches its serial console, and
returns its guest backing and virtualization tables to the host allocator. VM 0
cannot be deleted when it owns the passthrough NIC because its active VT-d
domain must remain installed until reboot. Memory may be specified in MiB by a
bare number or with `M`, `MB`, or `MiB`; `G`, `GB`, and `GiB` are also accepted.
The minimum is 256
MiB because the bundled UEFI Linux image cannot boot reliably below it. The
upper limit is calculated at runtime from currently free host RAM, outstanding
VM memory commitments, the management reserve, and the firmware-addressable
memory range. `info memory` reports the current limit for a newly created VM.

`vm list` and `info vm [ID]` report each VM's accessed working set, configured
RAM, allocated host backing, and cumulative CPU usage since creation. CPU usage
is measured from TSC cycles spent executing that VCPU. Memory usage is not
Linux's internal free-page percentage.

The same `nel>` management shell is available on the hypervisor's physical
COM1 console as soon as kernel initialization completes. This local console
remains available if the network device fails. `exit` closes a TCP session; on
the non-disconnectable local console it simply returns another prompt. Pressing
Enter on an empty line also redraws `nel>`.

The embedded TCP server accepts up to four simultaneous management connections.
Each client has independent command, response, retransmit, and idle-timeout
state, so one slow or idle client does not block the other management shells.

DHCP leases are renewed at T1 and rebound at T2 while the current address and
management TCP sessions remain active. Sessions are reset only if the DHCP
server rejects the lease or the lease actually expires without renewal.

While attached to a VM serial port, press `Ctrl-]` to detach and return to
the `nel>` prompt. Guest COM1 output is hidden by default and is sent only to
management consoles that explicitly ran `serial attach` or
`vm start --attach`; in particular, a TCP attachment does not expose guest
output on the physical hypervisor console. Only one console can own one VM
serial stream at a time; another console or a request for another VM is rejected
until the owner detaches. The shared bridge is activated only during that VM's
time slice so serial bytes cannot cross VM boundaries. `info runtime` reports
any guest-output bytes dropped because a slow TCP client exhausted the bounded
serial buffers.

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
and user-mode networking. QEMU uses `tap-nel0` for the hypervisor and
`tap-nel1` for VM 0, both attached to a Linux bridge named `br0` without
invoking QEMU's bridge helper. The bridge must already be
connected to a physical LAN that provides DHCP:

```text
hypervisor virtio-net -> tap-nel0 -> br0 -> physical NIC -> LAN DHCP server
VM 0 virtio-net       -> tap-nel1 -> br0 -> physical NIC -> LAN DHCP server
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
cd /home/mii/nas-work/nel_os_uefi
./nel_os_bootloader/setup-br0.sh up
NEL_OS_NET_MODE=bridge ./run.sh
```

The setup script only creates `tap-nel0` and `tap-nel1`; it never creates
`br0`, changes a physical interface, starts a DHCP server, or installs
firewall/NAT rules. The
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

`NEL_OS_NET_BRIDGE`, `NEL_OS_NET_TAP`, and `NEL_OS_GUEST_NET_TAP` can override
the default bridge and interface names. `NEL_OS_NET_MAC` and
`NEL_OS_GUEST_NET_MAC` override the default `52:54:00:12:34:56` and
`52:54:00:12:34:57` addresses; give every simultaneously running instance on
the same LAN unique locally administered MAC addresses.

To remove a tap created by the setup script without changing the physical
bridge:

```sh
./nel_os_bootloader/setup-br0.sh down
```
