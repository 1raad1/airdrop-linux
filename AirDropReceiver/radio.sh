#!/usr/bin/bash
# Privileged radio setup only. Files and desktop UI run as the logged-in user.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
[[ $EUID -eq 0 ]] || { echo 'Radio setup requires administrator authentication.'; exit 1; }
iface=${1:-wlan0}
backend=${2:-auto}
strategy=${3:-auto}
[[ $backend =~ ^(auto|owl|filin)$ ]] || { echo 'Backend must be auto, owl or filin.'; exit 1; }
[[ $strategy =~ ^(auto|pin|verbatim|rotate|widen|intersect)$ ]] || { echo 'Invalid channel strategy.'; exit 1; }
monitor=airdropmon
[[ $iface =~ ^[a-zA-Z0-9_.-]+$ && -d /sys/class/net/$iface/phy80211 ]] || { echo 'A valid Wi-Fi interface is required.'; exit 1; }
if iw dev "$iface" link | rg -q '^Connected'; then
    echo 'Wi-Fi is connected. Disconnect it first; use Ethernet during AirDrop.'
    exit 1
fi
if ip link show awdl0 >/dev/null 2>&1 || ip link show "$monitor" >/dev/null 2>&1; then
    echo 'An AirDrop radio session already exists.'
    exit 1
fi
managed=$(nmcli -g GENERAL.NM-MANAGED device show "$iface")
flags=$(cat "/sys/class/net/$iface/flags")
was_up=$(( flags & 1 ))
power_save=$(iw dev "$iface" get power_save | awk '{print $3}')
driver=$(basename "$(readlink -f "/sys/class/net/$iface/device/driver")")
radio_mode=plain
radio_pid=''
capture_pid=''
tcp_rule=0
mdns_rule=0
firewall_rule() {
    ip6tables -w 5 "$1" INPUT -i awdl0 -s fe80::/10 -p "$2" --dport "$3" \
        -m comment --comment codex-airdrop-session -j ACCEPT
}
cleanup() {
    trap - EXIT INT TERM HUP
    if (( tcp_rule )); then firewall_rule -D tcp 8771 || true; fi
    if (( mdns_rule )); then firewall_rule -D udp 5353 || true; fi
    if [[ -n $capture_pid ]]; then
        kill "$capture_pid" 2>/dev/null || true
        wait "$capture_pid" 2>/dev/null || true
    fi
    if [[ -n $radio_pid ]]; then
        kill "$radio_pid" 2>/dev/null || true
        wait "$radio_pid" 2>/dev/null || true
    fi
    iw dev "$monitor" del 2>/dev/null || true
    iw dev "$iface" set power_save "$power_save" 2>/dev/null || true
    if (( was_up )); then ip link set "$iface" up; else ip link set "$iface" down; fi
    nmcli device set "$iface" managed "$managed" || true
    echo RADIO_STOPPED
}
trap cleanup EXIT
trap 'exit 0' INT TERM HUP
# Make BBR available to the sender's own sockets without changing the
# system's default congestion control. Optional on kernels without it.
modprobe tcp_bbr 2>/dev/null || true
# Runtime rules only, scoped to IPv6 link-local peers on the temporary TAP.
# Do not change UFW configuration or open these ports on the LAN.
firewall_rule -I tcp 8771
tcp_rule=1
firewall_rule -I udp 5353
mdns_rule=1
nmcli device set "$iface" managed no
ip link set "$iface" down
if iw dev "$iface" interface add "$monitor" type monitor flags active; then
    echo 'ACTIVE_MONITOR_SUPPORTED'
else
    echo 'ACTIVE_MONITOR_UNSUPPORTED: using plain monitor'
    iw dev "$iface" interface add "$monitor" type monitor
fi
ip link set "$monitor" up
if [[ $backend == auto ]]; then
    if [[ $driver == rtw89* ]]; then backend=owl; else backend=filin; fi
fi
if [[ $strategy == auto ]]; then
    if [[ $driver == rtw89* ]]; then strategy=pin; else strategy=verbatim; fi
fi
if [[ $backend == owl ]]; then
    # Plain monitor is the verified configuration on this RTL8852CE.
    echo 'RADIO_MODE=plain'
    ./bin/owl -i "$monitor" -c 44 -h awdl0 -N -f -S "$strategy" -v </dev/null &
else
    RUST_LOG=filin_rs=debug,info ./bin/filin -i "$monitor" -c 44 -h awdl0 -N -f --no-force-master </dev/null &
fi
radio_pid=$!
for (( n=0; n<100; n++ )); do
    if ip -6 address show dev awdl0 2>/dev/null | rg -q 'inet6 fe80:.* scope link'; then
        echo RADIO_READY
        break
    fi
    kill -0 "$radio_pid" || { echo 'Radio process exited.'; exit 1; }
    sleep 0.2
done
ip -6 address show dev awdl0 | rg -q 'inet6 fe80:.* scope link' || { echo 'Radio setup timed out.'; exit 1; }
# Headers only: observe discovery and connection attempts on our own AWDL link.
tcpdump -l -nn -v -i awdl0 'udp port 5353 or tcp port 8770 or tcp port 8771 or icmp6' </dev/null &
capture_pid=$!
# The launcher owns stdin. Closing it or sending Stop restores the radio.
# A maximum duration also handles an abandoned desktop session.
while IFS= read -r -t 930 command; do
    case "$command" in
        Stop) break ;;
        KeepAlive) : ;;
        owl|plain|mixed)
            kill "$capture_pid" 2>/dev/null || true
            wait "$capture_pid" 2>/dev/null || true
            kill "$radio_pid" 2>/dev/null || true
            wait "$radio_pid" 2>/dev/null || true
            if [[ $command == plain ]]; then
                ip link set "$iface" down
                radio_mode=plain
            elif [[ $command == mixed ]]; then
                ip link set "$iface" up
                iw dev "$iface" set power_save off
                radio_mode=mixed
            fi
            echo "RADIO_MODE=$radio_mode"
            ./bin/owl -i "$monitor" -c 44 -h awdl0 -N -f -S "$strategy" -v </dev/null &
            radio_pid=$!
            for (( n=0; n<100; n++ )); do
                if ip -6 address show dev awdl0 2>/dev/null | rg -q 'inet6 fe80:.* scope link'; then break; fi
                sleep 0.2
            done
            tcpdump -l -nn -v -i awdl0 'udp port 5353 or tcp port 8770 or tcp port 8771 or icmp6' </dev/null &
            capture_pid=$!
            ;;
    esac
done
