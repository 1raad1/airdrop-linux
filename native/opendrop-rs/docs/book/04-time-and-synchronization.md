# Chapter 4 — Time: Availability Windows and Synchronization

If Chapter 3 was about *space* — the layout of bytes in a frame — this chapter is about *time*, which is the dimension AWDL really lives in. The protocol's central trick is to make two single-radio devices appear on the same channel at the same instant. That requires them to share a common, fine-grained sense of time and to agree on a schedule of when each will be listening. This chapter builds that timing model from the ground up: the unit of time, the windows, the master clock, the synchronization equation, and the unglamorous engineering realities (guard intervals, channel-switch costs) that eat into the ideal.

## The unit: the TU

AWDL inherits 802.11's time unit, the **TU (Time Unit)**, defined as **1024 microseconds** — close to but deliberately not exactly a millisecond (it's a power-of-two number of microseconds, which is convenient for hardware counters). Every duration in the protocol is expressed in TUs. Keep the conversion handy: 1 TU ≈ 1.024 ms, so 1000 TU ≈ 1.024 s.

## The window: the AW

The fundamental slot is the **Availability Window (AW)**, and in every shipping implementation an AW is **16 TUs** long (≈ 16.4 ms). During an AW, a node may be available to communicate. Outside its AWs, a node is free to do something else with its single radio — most importantly, tune back to its infrastructure AP's channel, or power the radio down.

The AW length is, like much of AWDL, a *configurable* value carried in the Synchronization Parameters TLV that is nonetheless **always 16** in practice. The protocol theoretically supports other configurations; Apple ships one.

## Presence mode and extension windows

A node doesn't have to be awake in *every* AW. The **presence mode** field says how often it listens. The only value Apple uses is **4**, meaning the node listens for **every fourth window**. The idea, in the abstract, is power saving: stay dark three windows out of four, wake for the fourth.

But there's a complication, and it's where the terminology gets thick. When a node is actively sending or receiving, it doesn't want to drop off the channel immediately after a single 16-TU window — that would chop transfers into tiny pieces. So AWDL allows an AW to be followed by **Extension Windows (EWs)**, also 16 TUs each, during which the node stays on the channel. With a presence mode of 4, there's room for **three EWs** after each AW.

In principle, AWs and EWs could be used differently — the protocol distinguishes them and even has separate configurable maxima for unicast, multicast, and action-frame extension windows (those `Max (3)` fields from Chapter 3). In practice, Apple sets all of them to 3 and uses the AW plus its three EWs as one indivisible **64-TU block**. SEEMOO coined a term for this block that the literature now uses universally: the **Extended Availability Window (EAW)** — one AW + three EWs = 4 × 16 = **64 TUs** ≈ 65.5 ms.

So although the protocol describes a rich hierarchy of windows with optional power-saving gaps, the deployed reality is simpler: **time is a stream of 64-TU EAWs.** This simplification is the single most important thing to internalize about AWDL timing, and it has a direct, costly consequence that we'll reach at the end of the chapter.

## Why EAWs instead of fine-grained windows? The channel-switch tax

Here's the engineering reason Apple collapsed everything into long EAWs, and it's a beautiful illustration of how hardware realities dominate protocol design.

Switching a Wi-Fi chip from one channel to another is **not instantaneous**. Measurements via the Broadcom debug utility put a channel switch at **at least ~8 ms (≈ 8 TU)**. On top of that, because clocks between nodes are never perfectly aligned, you need a **guard interval** at the edges of a window to absorb the residual synchronization error — about **3 TU** (more on why 3 below).

Now do the arithmetic on a bare 16-TU AW, assuming you wanted to reserve the EWs for a power-saving sleep:

- 16 TU window
- minus ~8 TU to switch onto the channel
- minus the guard interval
- ≈ **2 TU** of actual usable airtime.

That is catastrophic — you'd spend almost the entire window switching and guarding, with almost nothing left to communicate. Temporal efficiency would be on the order of 12.5%.

Now do it with a 64-TU EAW: you pay the ~8 TU switch cost and the guard interval **once** for the whole 64-TU block, leaving the large majority — better than ~78% — for communication. By amortizing the fixed switch-and-guard overhead across a four-times-longer window, EAWs turn a hopeless ~12.5% efficiency into a workable ~78%.

The trade is explicit and deliberate: **Apple sacrificed the deep-sleep power-saving opportunity (the EWs) in exchange for robustness and throughput.** Those EWs *could* have been radio-off time; instead they're kept on so that the channel-switch tax is paid rarely. This is the concrete mechanism behind Chapter 1's observation that AWDL is far less power-thrifty than its design rhetoric suggested. The power savings were real on paper and traded away in practice.

## The master clock

For all those EAWs to line up across devices, every node must share a sense of *when* each EAW starts. AWDL handles this with a single timing authority — the **master** — whose one essential job (per Chapter 5) is to emit a clock signal that everyone else follows.

The mechanism is announcement-based. In each action frame, a node (ultimately deriving its timing from the master) advertises two things from the Synchronization Parameters TLV:

- **`t_AW`** — the number of TUs remaining until the next EAW begins.
- **`i`** — the sequence number of the current AW/EW.

A receiver hearing this learns "the next window boundary is `t_AW` TUs from when this frame was created, and we're currently in window number `i`." From that, it can place its own clock onto the master's grid. The sequence number `i` is what later lets a node compute *which channel* a peer will be on in any given window (Chapter 6's formula consumes `i`).

## The synchronization equation

Now the precise part. A receiver wants to compute `T_AW`, the wall-clock instant when the next availability window starts, in its own local time. The naive answer — "now plus `t_AW` TUs" — is wrong, because of the gap between when the sender *computed* `t_AW` and when the frame actually hit the air. The two timestamps from the fixed header (Chapter 3) exist precisely to correct for that gap.

Let:

- `t_AW` = TUs to next EAW, as announced (so `t_AW · 1024` µs).
- `T_Tx,Target` = timestamp the sender set when the frame/timing was *created*.
- `T_Tx,PHY` = timestamp the sender set when the frame was *queued for transmission*.
- `t_air` = over-the-air propagation time.
- `T_Rx` = receiver's local time when it received the frame.

The receiver estimates the next window start as:

```
T_AW = t_AW · 1024  −  (T_Tx,PHY − T_Tx,Target)  +  t_air  +  T_Rx
```

In words: take the announced countdown, **subtract the sender's internal transmission delay** (the difference between the two timestamps — the time the frame spent waiting between creation and transmission), add the airtime, and anchor it to the moment of reception. The subtraction of `(T_Tx,PHY − T_Tx,Target)` is the heart of it: it removes the sender-side latency so that the countdown is measured from the right reference point.

Two simplifications the implementation makes:

- **`t_air` is ignored.** Over typical close-range Wi-Fi distances, propagation is sub-microsecond — negligible against a 3 ms error budget. So the term is dropped.
- **The accepted synchronization error is 3 ms** (≈ 3 TU). The driver tracks a "misalignment" metric: if a fresh computation of `T_AW` differs from a previous projection by more than ~3 ms, it counts as misaligned. That 3 TU tolerance is exactly the guard interval we budgeted above.

## How well does it actually work?

SEEMOO measured this empirically over a 20-minute idle run with several Apple devices, and the result is reassuring with an instructive caveat. The synchronization error between a slave and its master approximates a **Gaussian** with a mean around **−0.45 TU** and a standard deviation near **1 TU**. The 3-TU target is met in **more than 99%** of cases. So in the steady state, synchronization works: nodes really do agree on window boundaries to within a few milliseconds almost all the time.

The caveat is the dominant *source* of the residual error, and it's a software placement bug rather than anything fundamental. Recall that the equation assumes `T_Tx,PHY` is stamped at the *true* moment of transmission — after the 802.11 medium-access backoff (DCF) has run and the frame is actually going out. But in the macOS driver, **both** timestamps are set early, inside the driver, *before* DCF backoff. So `T_Tx,PHY` doesn't capture the variable medium-access delay at all, and that uncorrected jitter is the main thing smearing out the synchronization distribution. The protocol *designed* a clean delay-compensation mechanism; the implementation undercut it by stamping the "transmission" time in the wrong place. It still works well enough — the error stays inside the guard band 99%+ of the time — but it's a textbook case of an implementation detail eroding a protocol's theoretical accuracy.

## The cost of the guard interval

Tie the timing model back to throughput, because there's a direct tax. Because the synchronization error needs a guard band of ~3 TU at *each* edge of the usable window, you lose `2 × 3 = 6` TU of every 64-TU EAW to guard intervals:

```
usable fraction ≈ 1 − (2 × 3 TU) / (64 TU) ≈ 90.6%
```

So even in the best case, roughly **9–10% of the channel is burned purely to absorb clock imprecision.** Improve the synchronization accuracy (e.g. by stamping `T_Tx,PHY` correctly, after DCF) and you could shrink the guard band and reclaim some of that — a concrete example of how the implementation's timestamp-placement shortcut has a measurable throughput cost, not just an abstract one.

## Putting the timeline together

Step back and view a single EAW as a filmstrip, which is the mental image to carry forward:

```
|<-- channel switch (~8 TU) -->|<- guard ->|<----- usable airtime ----->|<- guard ->|
|<---------------------------------- 64 TU EAW ---------------------------------->|
        AW          EW          EW          EW
```

The node arrives on the channel (paying the switch tax once), waits out a guard interval to be sure it's aligned with peers, communicates through the bulk of the window, leaves another guard interval at the end, and then — if its channel sequence says so — switches to a different channel (perhaps back to the infrastructure AP) for the next EAW. Multiply this by the channel sequence, and you get the full hopping pattern that Chapter 6 lays out. But the atom of the whole system is this one 64-TU window, synchronized across the cluster to within a few milliseconds by a master's announced countdown, corrected by two timestamps, and fenced with guard intervals on each side.

That atom is what makes "two single-radio devices on the same channel at the same time" go from impossible to routine.

---

*Next: Chapter 5 — who gets to be the master, the surprisingly random election metric, how clusters merge, how loops are prevented in the sync tree, and how the protocol shrugs off a master that vanishes.*
