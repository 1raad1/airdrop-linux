# Chapter 6 — Channel Sequences and Coexistence

We now have synchronized time (Chapter 4) and an agreed master to anchor it (Chapter 5). This chapter spends that synchronization on its actual purpose: deciding **which channel** each node's single radio is tuned to in **each** window, so that peers can rendezvous *and* so that a node can still service its infrastructure Wi-Fi connection. This is where AWDL's headline trick — concurrent peer-to-peer and infrastructure networking on one radio — becomes concrete and measurable. We'll derive the channel-selection formula, walk the channel-allocation states that scale with traffic, and look at the throughput numbers that quantify the coexistence cost.

## The social channels

First, the meeting ground. AWDL designates **fixed social channels** where coordination happens: channel **6** in the 2.4 GHz band, and channel **44** in 5 GHz (with **149** also used depending on regulatory region). Channel **44** is the primary AWDL channel; **6** is the secondary. A node bootstrapping its interface listens on these social channels to find an existing cluster, and even under heavy AWDL load the sequence reserves a recurring visit to channel 6 — apparently for backward compatibility, so that older or 2.4-GHz-only peers can still meet everyone else somewhere predictable.

## The channel sequence

Each node advertises a **channel sequence**: an ordered list that says, for each window, which channel its radio will be on. This rides in the Channel Sequence TLV (type 18) and is also embedded in the Synchronization Parameters TLV. The structure (from Chapter 3): a length `c`, an encoding, a duplicate count, a **step** field, a **fill channel** (`0xffff`), and the channel list itself.

The deployed configuration fixes two numbers:

- **`c = 15`**, so there are `c + 1 = 16` channel entries in the list.
- **`step = 3`**.

The `step` field stretches each entry across multiple windows, exactly analogous to the presence mode stretching availability. With `step = 3`, **each channel entry spans 4 windows** — i.e. one entry covers a full EAW (recall an EAW is 4 × 16 TU = 64 TU from Chapter 4). So the 16 entries, each spanning 4 windows, cover:

```
(c + 1) · (step + 1) = 16 · 4 = 64 windows
```

And 64 windows × 16 TU = 1,048,576 µs ≈ **1 second**. So a full channel sequence is a **one-second, 64-window pattern** that then repeats. The whole rhythm of an AWDL radio is this one-second loop of "which channel am I on now."

(A caveat from the captures: the `step`-based stretching only works when the fill channel field is `0xffff`, which it always was in observed traffic.)

## The channel-selection formula

Here is the piece that ties time to space. Given the current availability-window **sequence number `i`** (the value the master announces and increments, from Chapter 4), any node can compute which channel a peer is on by indexing into that peer's advertised channel sequence:

```
C = i mod ((c + 1) · (step + 1))
```

With Apple's fixed values, that's `C = i mod 64` — i.e. the position within the 64-window cycle — which then selects the corresponding entry from the channel list (accounting for the 4-window span per entry). The beauty of this is what it enables: because every node knows the master's window number `i`, and because each node *broadcasts its own channel sequence*, **any node can compute, for any peer and any window, whether the two of them will be on the same channel at the same time.** That shared-channel-in-the-same-window condition is exactly the rendezvous opportunity. To send to a peer, you find the windows where your sequence and theirs coincide on a channel, and you transmit then.

This is the entire mechanism of AWDL rendezvous, reduced to a modular arithmetic lookup over a one-second table that everyone publishes. No negotiation, no handshake to agree on a channel — just "publish your schedule, read mine, and meet where they overlap."

## Coexistence: sharing the radio with infrastructure Wi-Fi

Now the payoff. The reason all this machinery exists is so your phone can run AWDL *without* dropping its connection to your home or office AP. The channel sequence is the instrument: by including the **AP's channel** in some of its window slots, a node carves out time to service the infrastructure link in between its AWDL windows.

The key insight is that the channel sequence is **adaptive to traffic load**. AWDL doesn't allocate a fixed split; it shifts the balance based on how much AWDL data is actually flowing. SEEMOO found references to **25 distinct allocation states** in the driver (including a real-time mode and various combinations). A representative subset, where `p` is the primary AWDL channel (44), `s` is the secondary (6), and `i` is the AP's channel:

| State | AWDL airtime | Channel list (16 entries) |
|---|---|---|
| Low Power | 25.0% | `p s p p` (pattern) |
| Idle | 37.5% | `p p p s p p` |
| Data+Infra | 50.0% | `p p p p i i i i s p p p i i i i` |
| (heavy + infra) | 75.0% | `p p p p p p i i s p p p p p p i i` |
| Data | 100.0% | `p p p p p p p p s p p p p p p p` |

Read these as the 16-entry channel lists, each entry spanning an EAW. Three properties stand out:

1. **At least 25% of the time is always allocated to AWDL** — even in the lowest-power state, the radio spends a quarter of its cycle on the peer channel. (This is, again, why AWDL's idle power consumption is higher than you'd hope: it's never truly dormant while active.)
2. **There's always a switch to channel 6** (the `s` slot, typically slot 9) — that recurring backward-compatibility visit to the secondary social channel.
3. **At least 25% of the time is reserved for the AP connection** (the `i` slots) *when the node is associated to one* — guaranteeing the infrastructure link gets serviced and doesn't starve.

The progression tells the coexistence story directly. When you're idle, AWDL takes a modest slice and leaves the radio mostly free for your home Wi-Fi. As an AirDrop transfer ramps up, AWDL escalates through the states — 37.5%, 50%, 75% — handing itself more windows and the AP fewer, until at full tilt (the "Data" state) it can claim **100%** of the sequence (still with the one mandatory channel-6 visit). The transfer you initiate literally reshapes the radio's time-sharing schedule in real time.

## The throughput cost of coexistence

How much does sharing one radio actually cost? SEEMOO measured TCP throughput across several configurations, and the numbers are clarifying (approximate, Mbit/s, hardware-dependent):

- **Only AWDL** (no AP): ~**758**
- **Only AP** (iMac as AP): ~**685** — comparable to AWDL-only, showing AWDL itself isn't the bottleneck; the link is limited by the devices' PHY rates, not by protocol overhead.
- **AWDL + AP on the *same* channel**: cumulative ~**773**, with bandwidth split roughly evenly between the two connections — essentially no penalty, because no channel switching is needed.
- **AWDL + AP on *different* channels**: cumulative throughput drops by about **13%** versus the same-channel case.

That **~13% hit** is the concrete price of the coexistence trick when AWDL and your AP are on different channels: the radio has to keep switching back and forth, paying the channel-switch tax (Chapter 4's ~8 TU) repeatedly, and that lost airtime shows up as reduced aggregate throughput. When they happen to share a channel, the penalty largely vanishes because there's nothing to switch between.

A curious secondary finding: in the different-channel case, the bandwidth was *no longer* evenly split between the AWDL and AP connections — AWDL got more. The researchers suspected AWDL was opportunistically using all three available spatial streams while the AP path used fewer. The details are hardware-specific, but the headline is robust: **coexistence is real and cheap when channels align, and costs you roughly an eighth of your throughput when they don't.**

## Why this design is the right shape

Step back and appreciate what the channel sequence accomplishes against the constraints from Chapter 1. A single radio cannot be in two places at once — but it *can* be in two places in rapid alternation, **if** every participant agrees precisely on the alternation schedule. The channel sequence is that schedule, made:

- **Self-describing** — each node publishes its own sequence, so no central coordinator has to assign channels.
- **Computable** — the simple `i mod 64` formula lets anyone predict anyone's channel in any window, enabling rendezvous with zero negotiation.
- **Adaptive** — the 25-state ladder lets the same mechanism serve a dormant interface cheaply and a saturating transfer aggressively, automatically.
- **Coexistence-aware** — by weaving the AP's channel into the sequence and guaranteeing it a minimum share, the node keeps its infrastructure link alive throughout.

The cost is the channel-switch tax (amortized by EAWs in Chapter 4) and the ~13% throughput penalty when channels differ. That's the bill for doing peer-to-peer and infrastructure networking on one radio — and it's a bill most users never see, because the whole apparatus is invisible behind a share sheet.

---

*Next: Chapter 7 — how AWDL folds mDNS/Bonjour service discovery into its action frames, how MAC randomization protects privacy, and how link-local IPv6 addresses are conjured without the usual neighbor-discovery handshake.*
