# AWDL synchronization notes (the lesson that made discovery work)

This is the single most important correctness item in the AWDL port. Get it
right from the start; it's what took the longest to diagnose in the C owl.

## The problem
AWDL devices exchange data only during synchronized **availability windows (AW)**.
A master announces, in its PSF/MIF action frames, `time_to_next_aw` and an
`aw_counter`. Each peer must predict the master's AW schedule in its own clock and
switch channels / transmit inside the window. If your sync error exceeds the AW
guard, data (mDNS, the HTTPS handshake) never crosses — even though you can still
*hear* the master's beacons. Symptom: owl follows the master fine, but nothing
reaches `awdl0`.

## owl's bug
`owl/src/rx.c` timestamped each received frame with `clock_gettime(MONOTONIC)` in
userspace — i.e. *after* the frame traversed the radio → USB → kernel → pcap →
userspace. On USB adapters that path has multi-millisecond, heavy-tailed jitter
(≈ TU-scale; 1 TU = 1024 µs). That jitter corrupts the sync estimate. On a PCIe/
DMA card (ath9k) the jitter is sub-TU, which is the real reason ath9k "just works".

Measured: Realtek RTL8822BU ≈ 12–21% of measurements out-of-threshold; Atheros
AR9170 (carl9170) ≈ **0.4%** — and the iPhone immediately discovered our receiver.

## The fix (port this, not owl's original)
Use the **radiotap TSFT** field — the card's hardware timestamp of the frame,
captured in the MAC at reception, immune to delivery jitter — as the frame's
"now". It's the card's *local* free-running TSF (verified: consistent across
different transmitters in one capture).

Because the scheduler/timers run in the host monotonic clock, bridge TSF→monotonic
with a filtered offset:
```
host_now = hw_tsft + true_offset + delivery_delay   (delivery_delay >= 0)
=> min(host_now - hw_tsft) tracks true_offset
```
Maintain the offset as an EMA with a sanity clamp (reject frames whose implied
host time deviates > ~100 ms — bogus TSF). Then `now = hw_tsft + offset` is a
jitter-free, host-domain timestamp. Feed THAT to the sync error computation.

(Our C patch used min-track first — it latched onto outliers and destabilized;
EMA + clamp was stable. Start with EMA + clamp.)

## Other radio lessons
- **Plain monitor mode**: many drivers reject `NL80211_MNTR_FLAG_ACTIVE`. Set
  plain monitor mode externally; don't request the active flag (owl's `-N`).
- **Channel**: anchor to **44** (5 GHz) for modern iPhones; AWDL also uses 6/149.
- **TX backpressure**: pcap/AF_PACKET inject can `EAGAIN` under load (we saw 32%
  loss on USB). Raise `SO_SNDBUF` / `net.core.wmem_*`, raise the iface txqueuelen,
  and queue/retry rather than drop.
- **Hardware**: Atheros `carl9170` clean; Realtek `rtw88` clones marginal + crash
  under injection. USB adapters drop off the bus under
  load and may need a driver reload.

## Verification
Log per-measurement sync error (TU and %) so it can be compared against the C owl
and across radios. "sync% low and stable" is the green light for discovery.
