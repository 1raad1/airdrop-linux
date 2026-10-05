#!/usr/bin/bash
# Privileged radio setup only. Files and desktop UI run as the logged-in user.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
[[ $EUID -eq 0 ]] || { echo 'Radio setup requires administrator authentication.'; exit 1; }
iface=${1:-wlan0}
backend=${2:-auto}
strategy=${3:-auto}
context=${4:-auto}
[[ $context =~ ^(auto|off|ap)$ ]] || { echo 'MAC context must be auto, off or ap.'; exit 1; }
[[ $backend =~ ^(auto|owl|filin)$ ]] || { echo 'Backend must be auto, owl or filin.'; exit 1; }
[[ $strategy =~ ^(auto|pin|verbatim|rotate|widen|intersect)$ ]] || { echo 'Invalid channel strategy.'; exit 1; }
monitor=airdropmon
[[ $iface =~ ^[a-zA-Z0-9_.-]+$ && -d /sys/class/net/$iface/phy80211 ]] || { echo 'A valid Wi-Fi interface is required.'; exit 1; }
if iw dev "$iface" link | rg -q '^Connected'; then
    echo 'Wi-Fi is connected. Disconnect it first; use Ethernet during AirDrop.'
    exit 1
fi
[[ $(iw dev "$iface" info | awk '$1 == "type" {print $2}') == managed ]] || {
    echo 'Use an unused managed Wi-Fi interface.'; exit 1;
}
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
context_pid=''
context_dir=''
tcp_rule=0
mdns_rule=0
stop_context() {
    if [[ -n $context_pid ]]; then
        kill "$context_pid" 2>/dev/null || true
        wait "$context_pid" 2>/dev/null || true
        context_pid=''
        ip link set "$iface" down || true
        iw dev "$iface" set type managed || true
    fi
    if [[ -n $context_dir ]]; then
        rm -rf -- "$context_dir"
        context_dir=''
    fi
}
start_context() {
    command -v wpa_supplicant >/dev/null || return 1
    # A private, hidden WPA2 context activates the hardware MAC on channel 44.
    # No DHCP, bridging or routing is configured here. AWDL stays on TAP.
    context_dir=$(mktemp -d /run/airdrop-context.XXXXXXXX) || return 1
    chmod 700 "$context_dir" || return 1
    local secret
    secret=$(openssl rand -hex 32) || return 1
    cat > "$context_dir/config" <<EOF || return 1
ap_scan=2
network={
    ssid="airdrop-context-${secret:0:12}"
    mode=2
    frequency=5220
    ignore_broadcast_ssid=1
    key_mgmt=WPA-PSK
    proto=RSN
    pairwise=CCMP
    group=CCMP
    psk=$secret
}
EOF
    chmod 600 "$context_dir/config" || return 1
    wpa_supplicant -D nl80211 -i "$iface" -c "$context_dir/config" > "$context_dir/log" 2>&1 &
    context_pid=$!
    for (( attempt=0; attempt<100; attempt++ )); do
        if rg -q 'AP-ENABLED' "$context_dir/log"; then
            echo 'RADIO_CONTEXT=ap'
            return 0
        fi
        kill -0 "$context_pid" 2>/dev/null || break
        sleep 0.1
    done
    return 1
}
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
    stop_context
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
if [[ $context == auto ]]; then
    # Only this driver/hardware combination has been verified with a phone.
    if [[ $driver == rtw89_8852ce && $backend == owl && $strategy == pin ]] && command -v wpa_supplicant >/dev/null; then
        context=ap
    else
        context=off
    fi
fi
if [[ $context == ap ]]; then
    if [[ $backend != owl || $strategy != pin ]]; then
        echo 'AP context requires OWL with the pin strategy on channel 44.'
        exit 1
    fi
    if ! start_context; then
        echo 'RADIO_CONTEXT_FAILED: falling back to plain monitor'
        stop_context
    fi
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
                stop_context
                ip link set "$iface" down
                radio_mode=plain
            elif [[ $command == mixed ]]; then
                stop_context
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
