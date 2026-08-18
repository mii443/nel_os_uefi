#!/usr/bin/env bash
set -euo pipefail

readonly BRIDGE="${NEL_OS_NET_BRIDGE:-br0}"
readonly TAP="${NEL_OS_NET_TAP:-tap-nel0}"
readonly GUEST_TAP="${NEL_OS_GUEST_NET_TAP:-tap-nel1}"
readonly RUNTIME_DIRECTORY=/run/nel-os-network
readonly TAPS=("${TAP}" "${GUEST_TAP}")

validate_interface_name() {
    local name=$1
    if [[ ! "${name}" =~ ^[[:alnum:]_.:-]{1,15}$ ]]; then
        echo "Invalid network interface name: ${name}" >&2
        exit 2
    fi
}

validate_interface_name "${BRIDGE}"
validate_interface_name "${TAP}"
validate_interface_name "${GUEST_TAP}"
if [[ "${TAP}" == "${GUEST_TAP}" ]]; then
    echo "Hypervisor and guest tap names must differ." >&2
    exit 2
fi

if [[ ${EUID} -ne 0 ]]; then
    if ! command -v sudo >/dev/null 2>&1; then
        echo "Root privileges are required and sudo is not installed." >&2
        exit 1
    fi
    exec sudo --preserve-env=NEL_OS_NET_BRIDGE,NEL_OS_NET_TAP,NEL_OS_GUEST_NET_TAP,NEL_OS_QEMU_USER "$0" "$@"
fi

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Required command is missing: $1" >&2
        exit 1
    fi
}

bridge_has_uplink() {
    local port_path
    local port
    local port_paths=("/sys/class/net/${BRIDGE}/brif/"*)

    for port_path in "${port_paths[@]}"; do
        [[ -e "${port_path}" ]] || continue
        port="$(basename "${port_path}")"
        if [[ "${port}" != "${TAP}" && "${port}" != "${GUEST_TAP}" && ! -e "/sys/class/net/${port}/tun_flags" ]]; then
            return 0
        fi
    done
    return 1
}

tap_master() {
    local tap=$1
    if [[ -e "/sys/class/net/${tap}/master" ]]; then
        basename "$(readlink -f "/sys/class/net/${tap}/master")"
    fi
}

configure_tap() {
    local tap=$1
    local qemu_user=$2
    local qemu_uid=$3
    local marker="${RUNTIME_DIRECTORY}/${tap}.created"
    local description
    local master

    if [[ ! -e "/sys/class/net/${tap}" ]]; then
        ip tuntap add dev "${tap}" mode tap user "${qemu_user}"
        mkdir -p "${RUNTIME_DIRECTORY}"
        touch "${marker}"
    elif [[ ! -e "/sys/class/net/${tap}/tun_flags" ]]; then
        echo "Interface ${tap} exists but is not a tap device." >&2
        exit 1
    else
        description="$(ip tuntap show | awk -v tap="${tap}:" '$1 == tap { print; exit }')"
        if [[ "${description}" != *" user ${qemu_uid}"* &&
            "${description}" != *" user ${qemu_user}"* ]]; then
            echo "Tap ${tap} is not owned by QEMU user ${qemu_user} (${qemu_uid})." >&2
            exit 1
        fi
    fi

    master="$(tap_master "${tap}")"
    if [[ -n "${master}" && "${master}" != "${BRIDGE}" ]]; then
        echo "Tap ${tap} is already attached to ${master}; refusing to move it." >&2
        exit 1
    fi
    ip link set dev "${tap}" master "${BRIDGE}"
    ip link set dev "${tap}" up
}

network_up() {
    local qemu_user="${NEL_OS_QEMU_USER:-${SUDO_USER:-}}"
    local qemu_uid
    local tap

    require_command ip
    if [[ -z "${qemu_user}" || "${qemu_user}" == root ]]; then
        echo "Set NEL_OS_QEMU_USER to the unprivileged account that runs QEMU." >&2
        exit 1
    fi
    if ! id "${qemu_user}" >/dev/null 2>&1; then
        echo "QEMU user ${qemu_user} does not exist." >&2
        exit 1
    fi
    qemu_uid="$(id -u "${qemu_user}")"

    if [[ ! -d "/sys/class/net/${BRIDGE}/bridge" ]]; then
        echo "Required Linux bridge ${BRIDGE} does not exist." >&2
        echo "Configure ${BRIDGE} with a physical upstream interface before running this script." >&2
        exit 1
    fi
    if ! bridge_has_uplink; then
        echo "Bridge ${BRIDGE} has no upstream interface." >&2
        echo "Attach a physical LAN interface to ${BRIDGE} before running this script." >&2
        exit 1
    fi

    for tap in "${TAPS[@]}"; do
        configure_tap "${tap}" "${qemu_user}" "${qemu_uid}"
    done
    ip link set dev "${BRIDGE}" up

    echo "Attached ${TAP} and ${GUEST_TAP} to ${BRIDGE}. The LAN DHCP server can configure both OS instances."
}

network_down() {
    local tap
    local marker
    require_command ip
    for tap in "${TAPS[@]}"; do
        marker="${RUNTIME_DIRECTORY}/${tap}.created"
        if [[ ! -e "${marker}" ]]; then
            echo "Tap ${tap} was not created by this script; leaving it unchanged."
            continue
        fi
        if [[ -e "/sys/class/net/${tap}/tun_flags" ]]; then
            ip link delete dev "${tap}"
        fi
        rm -f "${marker}"
        echo "Removed ${tap}; bridge ${BRIDGE} was not changed."
    done
    rmdir "${RUNTIME_DIRECTORY}" 2>/dev/null || true
}

case "${1:-up}" in
    up)
        network_up
        ;;
    down)
        network_down
        ;;
    *)
        echo "Usage: $0 [up|down]" >&2
        exit 2
        ;;
esac
