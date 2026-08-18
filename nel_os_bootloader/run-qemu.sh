#!/bin/bash -ex

EFI_BINARY="$1"
readonly NET_MODE="${NEL_OS_NET_MODE:-user}"
readonly NET_BRIDGE="${NEL_OS_NET_BRIDGE:-br0}"
readonly NET_TAP="${NEL_OS_NET_TAP:-tap-nel0}"
readonly NET_MAC="${NEL_OS_NET_MAC:-52:54:00:12:34:56}"
readonly NET_BIND_ADDRESS="${NEL_OS_NET_BIND_ADDR:-127.0.0.1}"
readonly NET_HOST_PORT="${NEL_OS_NET_HOST_PORT:-5555}"

if [[ ! "${NET_MAC}" =~ ^([[:xdigit:]]{2}:){5}[[:xdigit:]]{2}$ ]]; then
    echo "Invalid virtio-net MAC address: ${NET_MAC}" >&2
    exit 2
fi

is_ipv4_address() {
    local address=$1
    local octet
    local octets
    IFS=. read -r -a octets <<<"${address}"
    [[ ${#octets[@]} -eq 4 ]] || return 1
    for octet in "${octets[@]}"; do
        [[ "${octet}" =~ ^[0-9]{1,3}$ ]] || return 1
        ((10#${octet} <= 255)) || return 1
    done
}

NET_ARGS=()
case "${NET_MODE}" in
    user)
        if ! is_ipv4_address "${NET_BIND_ADDRESS}"; then
            echo "Invalid host-forward bind address: ${NET_BIND_ADDRESS}" >&2
            exit 2
        fi
        if [[ ! "${NET_HOST_PORT}" =~ ^[0-9]{1,5}$ ]] ||
            ((10#${NET_HOST_PORT} < 1 || 10#${NET_HOST_PORT} > 65535)); then
            echo "Invalid host-forward management port: ${NET_HOST_PORT}" >&2
            exit 2
        fi
        NET_ARGS=(
            -netdev
            "user,id=hypervisor_net,net=10.0.2.0/24,dhcpstart=10.0.2.15,hostfwd=tcp:${NET_BIND_ADDRESS}:${NET_HOST_PORT}-10.0.2.15:5555,hostfwd=udp:${NET_BIND_ADDRESS}:${NET_HOST_PORT}-10.0.2.15:5555"
        )
        ;;
    bridge)
        if [[ ! "${NET_BRIDGE}" =~ ^[[:alnum:]_.:-]{1,15}$ ]] ||
            [[ ! "${NET_TAP}" =~ ^[[:alnum:]_.:-]{1,15}$ ]]; then
            echo "Invalid bridge or tap interface name." >&2
            exit 2
        fi
        if [[ ! -d "/sys/class/net/${NET_BRIDGE}/bridge" ]]; then
            echo "Required bridge ${NET_BRIDGE} is not configured." >&2
            echo "Configure a physical LAN bridge, then run ./setup-br0.sh up." >&2
            exit 1
        fi
        if [[ ! -e "/sys/class/net/${NET_TAP}/tun_flags" ]]; then
            echo "Required tap ${NET_TAP} is not configured." >&2
            echo "Run ./setup-br0.sh up before starting QEMU." >&2
            exit 1
        fi
        if [[ "$(basename "$(readlink -f "/sys/class/net/${NET_TAP}/master")")" != "${NET_BRIDGE}" ]]; then
            echo "Tap ${NET_TAP} is not attached to required bridge ${NET_BRIDGE}." >&2
            echo "Run ./setup-br0.sh up before starting QEMU." >&2
            exit 1
        fi

        BRIDGE_HAS_UPLINK=false
        for port_path in "/sys/class/net/${NET_BRIDGE}/brif/"*; do
            [[ -e "${port_path}" ]] || continue
            port="$(basename "${port_path}")"
            if [[ "${port}" != "${NET_TAP}" && ! -e "/sys/class/net/${port}/tun_flags" ]]; then
                BRIDGE_HAS_UPLINK=true
                break
            fi
        done
        if [[ "${BRIDGE_HAS_UPLINK}" != true ]]; then
            echo "Required bridge ${NET_BRIDGE} has no physical upstream interface." >&2
            exit 1
        fi
        NET_ARGS=(-netdev "tap,id=hypervisor_net,ifname=${NET_TAP},script=no,downscript=no")
        ;;
    *)
        echo "Invalid NEL_OS_NET_MODE: ${NET_MODE} (expected user or bridge)" >&2
        exit 2
        ;;
esac

./clean.sh
./create-iso.sh "$EFI_BINARY"

QEMU_ACCEL_ARGS=()
if [[ -r /dev/kvm && -w /dev/kvm ]]; then
    QEMU_ACCEL_ARGS=(-enable-kvm -cpu host)
else
    QEMU_ACCEL_ARGS=(-machine accel=tcg -cpu max,+svm)
fi

run_qemu() {
    local debug_args=()
    if [[ -n "${NEL_OS_QEMU_GDB:-}" ]]; then
        debug_args=(-s)
    fi

    qemu-system-x86_64 "$@" \
        -m 1G \
        -serial mon:stdio \
        -nographic \
        -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE.fd \
        -drive if=pflash,format=raw,readonly=on,file=OVMF_VARS.fd \
        -cdrom nel_os.iso \
        -boot d \
        -smp 1 \
        "${NET_ARGS[@]}" \
        -device "virtio-net-pci,netdev=hypervisor_net,disable-modern=on,mac=${NET_MAC}" \
        "${debug_args[@]}" \
        --no-shutdown --no-reboot
}

run_qemu "${QEMU_ACCEL_ARGS[@]}"
