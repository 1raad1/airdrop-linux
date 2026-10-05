# Chapter 2 — Architecture and the Five Phases

Chapter 1 left us with a set of requirements and the rough shape of an answer. This chapter turns that shape into an architecture. We'll do two things: first, locate AWDL inside the operating system — which processes and drivers actually implement it — and second, walk the **five phases** of a link's life cycle, from the moment an app asks for peer-to-peer connectivity to the moment bytes are flowing. These five phases are the skeleton the rest of the book hangs flesh on; every later chapter is, in effect, a deep zoom into one of them.

## Where AWDL lives in the OS

A useful surprise from the reverse-engineering work: AWDL is not some small user-space daemon. The **bulk of the protocol stack lives in the kernel**, inside the Wi-Fi driver — and that Wi-Fi driver is the *largest* kernel extension in macOS. This matters enormously for the security story later: a parser with this much attack surface, sitting in the kernel, reachable over the air, is a target-rich environment.

The driver comes in two parts, and the split is meaningful:

- **`IO80211Family`** — the generic, device-independent Wi-Fi driver. This is where most AWDL **frame parsing and creation** happens, and where the AWDL **state machine** is maintained. When an action frame arrives and its TLVs need to be torn apart and acted on, that's here.
- **A device-specific driver** (historically `AirportBrcm4360`, later `AirportBrcmNIC` for Broadcom chips) — this handles the **time-critical** functions, above all the precise **synchronization** that keeps availability windows aligned. Timing has to be close to the metal, so it lives with the hardware-specific code.

Above the kernel sits a constellation of user-space frameworks and daemons that drive AWDL indirectly. A simplified map of the macOS side:

- **`sharingd`** — the user-facing sharing daemon; the thing that actually runs AirDrop's logic.
- **`mDNSResponder`** — Apple's multicast-DNS / DNS-SD implementation, i.e. **Bonjour**. AWDL's service discovery is Bonjour offloaded onto Wi-Fi frames, so this is a central player.
- **`CoreWLAN` / `airportd` / `Apple80211`** — the Wi-Fi control plane.
- **`NSNetService` (Foundation)** — the public API through which third-party apps advertise and browse services; using it is what indirectly *activates* the AWDL interface for an ordinary app.
- **`WirelessProximity` / `wirelessproxd` and `CoreBluetooth` / `blued`** — the BLE side, used (among other things) to trigger AWDL activation for AirDrop.

The virtual network interface that user space sees is **`awdl0`**. From the IP stack's point of view, `awdl0` is just another interface with a link-local IPv6 address; the kernel quietly translates between IPv6 packets on `awdl0` and AWDL data frames on the air. This is a clean abstraction: applications speak ordinary IPv6 sockets and never have to know about availability windows or channel sequences.

One important consequence of the `awdl0` design, which we'll revisit under security: Apple installs a **default packet filter** so that ordinary services don't accidentally start listening on `awdl0` and become reachable by any unauthenticated stranger nearby. To deliberately expose a service over AWDL you must opt in through the dedicated API. The interface is also brought up **only on demand** and torn down once traffic stops — partly for power, partly (perhaps accidentally) for security, since it shrinks the window during which the device is attackable.

## On-demand by design

That last point deserves emphasis because it shapes the whole life cycle: **AWDL is off by default.** It is not a network you are persistently joined to. It is a capability that flares to life when something needs it and goes dark again afterward. Activation is *requested*, often only temporarily, by an application or system service. Examples:

- **AirDrop** uses **BLE** to activate AWDL: when you open the share sheet, the device starts broadcasting (over BLE) truncated hashes of your contact identifiers, and nearby devices that hear a relevant signal wake their AWDL interfaces.
- **AirPlay** receivers — an Apple TV, say — keep AWDL **constantly advertising** their presence, because they want to be discoverable at all times.
- A **third-party app** that advertises or browses a service via `NSNetService` activates the interface *indirectly*, without ever naming AWDL.

So the trigger varies, but the pattern is constant: some higher-layer intent lights up the interface, the five phases run, data moves, and then — assuming nothing keeps it busy — the interface quiesces.

## The five phases

With the OS map in hand, here is the life cycle. Think of these as phases, not strictly sequential stages — several overlap and recur continuously while a link is alive — but the ordering is the natural causal one.

### Phase 1 — Activation

Something requests AWDL. The interface comes up. As described above, the request can come from BLE (AirDrop), from a constantly-advertising receiver (AirPlay), or from an app using the service APIs. The key idea is that this is *demand-driven*: nothing about AWDL runs until a real need appears, which is why your phone isn't constantly burning power maintaining a peer-to-peer mesh.

### Phase 2 — Master Election

Once active, a node needs to know whose clock to follow. AWDL designates fixed **social channels** — channel **6** in 2.4 GHz and **44** (and, depending on region, **149**) in 5 GHz — as the meeting ground for coordination. A freshly activated node tunes to the social channels and **listens** for a couple of seconds. If it hears coordination frames (specifically Periodic Synchronization Frames, introduced next chapter) from an existing cluster, it can **adopt that cluster's master**. If it hears nothing, it appoints **itself** master and starts emitting timing. Election is covered in full in Chapter 5; for now, the takeaway is that exactly one node in a cluster is the timing authority, and joining nodes either find it or become it.

### Phase 3 — Synchronized Channel Sequences

This is the heart of the protocol and the subject of Chapters 4 and 6. AWDL structures time into a repeating sequence of fixed-length slots called **Availability Windows (AWs)**. For each slot, a node advertises whether it will be available and, if so, on which channel its radio will be tuned. Every node broadcasts its own **channel sequence**; a peer compares that sequence against its own. Wherever two peers' sequences put them on the **same channel in the same window**, communication during that window is possible. The master's job is to keep everyone's clocks aligned so that "the same window" actually means the same wall-clock moment across the cluster. Get this right and two single-radio devices can rendezvous reliably; get the clock alignment wrong and they simply never hear each other.

### Phase 4 — Service Discovery

AWDL piggybacks **DNS Service Discovery (DNS-SD / Bonjour)** directly onto its coordination frames. Rather than running a separate discovery exchange, a node staples its service records — the Bonjour PTR/SRV/TXT data — right into the action frames it is already broadcasting for synchronization. The upshot is that services are discovered essentially the instant a node changes its advertisements: there is no extra round trip, no separate discovery protocol spinning up. Chapter 7 covers exactly how the mDNS records are encoded into TLVs and how addressing works.

### Phase 5 — Data Transfer

Finally, user data. AWDL carries data in its own **data-frame format**, and the only network-layer protocol it transports is **IPv6**. To send to a particular peer, a node computes the windows during which *both* devices are tuned to the same channel, and transmits only during those windows. Crucially, AWDL **adapts its channel sequence to traffic load**: when idle it allocates only a slice of time to the peer channel (leaving room for the infrastructure connection), but as you push more data it can dedicate progressively more — up to the entire sequence — to AWDL. That adaptivity is what lets a 100 MB AirDrop saturate the link while an idle interface stays cheap. Chapter 6 details the allocation states.

## A worked mental model: a 100 MB AirDrop

Stitch the phases into a single story, because seeing them in motion is worth more than the list. Researchers captured exactly this — a 100 MB file transfer over AirDrop — and the trace tells the whole tale:

1. You open the share sheet. **(Activation)** Your phone starts emitting BLE advertisements with truncated contact-identifier hashes; the receiver, hearing a relevant signal, wakes its `awdl0`.
2. Both devices land on the social channels and sort out timing. **(Election + Synchronization)** One becomes — or already is — master; the other aligns its availability windows to it. On the wire you'd see coordination frames on channel 6 and channel 44.
3. The receiver's Bonjour records, riding inside those same coordination frames, tell your phone "here is an AirDrop-capable peer, here's how to reach it." **(Service Discovery)**
4. An IPv6 link comes up between the two `awdl0` interfaces; on top of it runs an ordinary HTTPS/TLS exchange (the AirDrop application protocol). As the bulk transfer begins, AWDL **shifts its channel sequence** to hand more and more airtime to the peer channel, and the data frames stream across. **(Data Transfer)** You'd see the activity migrate onto the data channel as throughput ramps.
5. The transfer finishes, traffic stops, and after a timeout `awdl0` goes back to sleep.

Every one of those steps is a rabbit hole, and the following chapters go down each in turn. But the five-phase skeleton — *activate, elect, synchronize, discover, transfer* — is the thing to keep in your head. Whenever a later detail threatens to feel arbitrary, ask which phase it serves, and it will usually snap back into place.

---

*Next: Chapter 3 gets concrete and byte-level — the action-frame and data-frame formats, the fixed header, and the complete TLV catalogue that carries every piece of control information in the protocol.*
