#!/usr/bin/env bash
set -euxo pipefail

readonly SOURCE_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly EFI_BINARY="$(realpath -- "$1")"
readonly NET_MODE="${NEL_OS_NET_MODE:-user}"
readonly NET_BRIDGE="${NEL_OS_NET_BRIDGE:-br0}"
readonly NET_TAP="${NEL_OS_NET_TAP:-tap-nel0}"
readonly GUEST_NET_TAP="${NEL_OS_GUEST_NET_TAP:-tap-nel1}"
readonly NET_MAC="${NEL_OS_NET_MAC:-52:54:00:12:34:56}"
readonly GUEST_NET_MAC="${NEL_OS_GUEST_NET_MAC:-52:54:00:12:34:57}"
readonly NET_BIND_ADDRESS="${NEL_OS_NET_BIND_ADDR:-127.0.0.1}"
readonly NET_HOST_PORT="${NEL_OS_NET_HOST_PORT:-5555}"
readonly BLOCK_SIZE_MIB="${NEL_OS_BLOCK_SIZE_MIB:-64}"
readonly LOCAL_CACHE_BASE="${NEL_OS_LOCAL_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME}/.cache}/nel_os_uefi}"
readonly RUNTIME_BASE="${NEL_OS_RUNTIME_BASE:-${XDG_RUNTIME_DIR:-/tmp}/nel_os_uefi-${UID}}"
HOST_SLUG="$(hostname -s | tr -cd '[:alnum:]_.-')"
readonly HOST_SLUG
readonly RUNTIME_DIR="${RUNTIME_BASE}/${HOST_SLUG}-${NET_MODE}-${NET_HOST_PORT}"

export CARGO_TARGET_DIR="${NEL_OS_CARGO_TARGET_DIR:-${CARGO_TARGET_DIR:-${LOCAL_CACHE_BASE}/target}}"

for mac in "${NET_MAC}" "${GUEST_NET_MAC}"; do
    if [[ ! "${mac}" =~ ^([[:xdigit:]]{2}:){5}[[:xdigit:]]{2}$ ]]; then
        echo "Invalid virtio-net MAC address: ${mac}" >&2
        exit 2
    fi
done
if [[ "${NET_MAC,,}" == "${GUEST_NET_MAC,,}" ]]; then
    echo "Hypervisor and guest virtio-net MAC addresses must differ." >&2
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
GUEST_NET_ARGS=()
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
        GUEST_NET_ARGS=(
            -netdev
            "user,id=guest_net,net=10.0.3.0/24,dhcpstart=10.0.3.15"
        )
        ;;
    bridge)
        if [[ ! "${NET_BRIDGE}" =~ ^[[:alnum:]_.:-]{1,15}$ ]] ||
            [[ ! "${NET_TAP}" =~ ^[[:alnum:]_.:-]{1,15}$ ]] ||
            [[ ! "${GUEST_NET_TAP}" =~ ^[[:alnum:]_.:-]{1,15}$ ]]; then
            echo "Invalid bridge or tap interface name." >&2
            exit 2
        fi
        if [[ ! -d "/sys/class/net/${NET_BRIDGE}/bridge" ]]; then
            echo "Required bridge ${NET_BRIDGE} is not configured." >&2
            echo "Configure a physical LAN bridge, then run ${SOURCE_DIR}/setup-br0.sh up." >&2
            exit 1
        fi
        for tap in "${NET_TAP}" "${GUEST_NET_TAP}"; do
            if [[ ! -e "/sys/class/net/${tap}/tun_flags" ]]; then
                echo "Required tap ${tap} is not configured." >&2
                echo "Run ${SOURCE_DIR}/setup-br0.sh up before starting QEMU." >&2
                exit 1
            fi
            if [[ "$(basename "$(readlink -f "/sys/class/net/${tap}/master")")" != "${NET_BRIDGE}" ]]; then
                echo "Tap ${tap} is not attached to required bridge ${NET_BRIDGE}." >&2
                echo "Run ${SOURCE_DIR}/setup-br0.sh up before starting QEMU." >&2
                exit 1
            fi
        done

        BRIDGE_HAS_UPLINK=false
        for port_path in "/sys/class/net/${NET_BRIDGE}/brif/"*; do
            [[ -e "${port_path}" ]] || continue
            port="$(basename "${port_path}")"
            if [[ "${port}" != "${NET_TAP}" && "${port}" != "${GUEST_NET_TAP}" && ! -e "/sys/class/net/${port}/tun_flags" ]]; then
                BRIDGE_HAS_UPLINK=true
                break
            fi
        done
        if [[ "${BRIDGE_HAS_UPLINK}" != true ]]; then
            echo "Required bridge ${NET_BRIDGE} has no physical upstream interface." >&2
            exit 1
        fi
        NET_ARGS=(-netdev "tap,id=hypervisor_net,ifname=${NET_TAP},script=no,downscript=no")
        GUEST_NET_ARGS=(-netdev "tap,id=guest_net,ifname=${GUEST_NET_TAP},script=no,downscript=no")
        ;;
    *)
        echo "Invalid NEL_OS_NET_MODE: ${NET_MODE} (expected user or bridge)" >&2
        exit 2
        ;;
esac

mkdir -p "${CARGO_TARGET_DIR}" "${RUNTIME_DIR}"

if [[ -n "${NEL_OS_BLOCK_IMAGE:-}" ]]; then
    if [[ ! -f "${NEL_OS_BLOCK_IMAGE}" ]]; then
        echo "Host block image does not exist or is not a regular file: ${NEL_OS_BLOCK_IMAGE}" >&2
        exit 2
    fi
    BLOCK_IMAGE="$(realpath -- "${NEL_OS_BLOCK_IMAGE}")"
    if [[ "$(dd if="${BLOCK_IMAGE}" bs=1 count=8 status=none)" != "NELBOOT1" ]]; then
        echo "Host block image is not a NEL Linux boot bundle: ${BLOCK_IMAGE}" >&2
        exit 2
    fi
else
    if [[ ! "${BLOCK_SIZE_MIB}" =~ ^[0-9]+$ ]] ||
        ((10#${BLOCK_SIZE_MIB} < 1 || 10#${BLOCK_SIZE_MIB} > 1048576)); then
        echo "Invalid NEL_OS_BLOCK_SIZE_MIB: ${BLOCK_SIZE_MIB}" >&2
        exit 2
    fi
    BLOCK_IMAGE="${RUNTIME_DIR}/host-block.img"
    if [[ -e "${BLOCK_IMAGE}" && ! -f "${BLOCK_IMAGE}" ]]; then
        echo "Default host block image is not a regular file: ${BLOCK_IMAGE}" >&2
        exit 2
    fi
    "${SOURCE_DIR}/create-linux-disk.sh" \
        "${BLOCK_IMAGE}" \
        "${BLOCK_SIZE_MIB}" \
        "${SOURCE_DIR}/bzImage" \
        "${SOURCE_DIR}/rootfs-n.cpio.gz"
fi
readonly BLOCK_IMAGE

"${SOURCE_DIR}/create-iso.sh" "${EFI_BINARY}" "${RUNTIME_DIR}"
cp "${SOURCE_DIR}/OVMF_VARS.fd" "${RUNTIME_DIR}/OVMF_VARS.fd"

QEMU_ACCEL_ARGS=()
if [[ -r /dev/kvm && -w /dev/kvm ]]; then
    QEMU_ACCEL_ARGS=(-enable-kvm -cpu host)
else
    QEMU_ACCEL_ARGS=(-accel tcg -cpu max,+svm)
fi

run_qemu() {
    local debug_args=()
    if [[ -n "${NEL_OS_QEMU_GDB:-}" ]]; then
        debug_args=(-s)
    fi

    qemu-system-x86_64 "$@" \
        -m 1G \
        -machine q35 \
        -device intel-iommu \
        -serial mon:stdio \
        -nographic \
        -drive "if=pflash,format=raw,readonly=on,file=${SOURCE_DIR}/OVMF_CODE.fd" \
        -drive "if=pflash,format=raw,file=${RUNTIME_DIR}/OVMF_VARS.fd" \
        -drive "if=none,id=hypervisor_block,format=raw,file=${BLOCK_IMAGE}" \
        -cdrom "${RUNTIME_DIR}/nel_os.iso" \
        -boot d \
        -smp 1 \
        "${NET_ARGS[@]}" \
        "${GUEST_NET_ARGS[@]}" \
        -device "virtio-net-pci,netdev=hypervisor_net,disable-modern=on,vectors=0,mac=${NET_MAC}" \
        -device "virtio-net-pci,netdev=guest_net,disable-legacy=on,iommu_platform=on,vectors=0,mac=${GUEST_NET_MAC}" \
        -device "virtio-blk-pci,drive=hypervisor_block,disable-modern=on,vectors=0" \
        "${debug_args[@]}" \
        --no-shutdown --no-reboot
}

run_qemu "${QEMU_ACCEL_ARGS[@]}"
