# Chapter 7 — Service Discovery and Addressing

A synchronized, channel-hopping link is useless until devices can find each other's *services* and address each other at the network layer. This chapter covers the two mechanisms that turn a raw AWDL link into something applications can use: **service discovery**, in which AWDL folds Bonjour directly into its action frames so that services are found essentially instantly; and **addressing**, in which AWDL conjures working IPv6 connectivity — complete with privacy-preserving MAC randomization — without the usual neighbor-discovery handshake. These are the glue between the link layer of Chapters 3–6 and the applications (like AirDrop) of Chapter 8.

## Bonjour, briefly

Apple's service-discovery stack is **Bonjour**, which is two standards working together: **mDNS** (multicast DNS — DNS-style name resolution on a local link with no DNS server) and **DNS-SD** (DNS Service Discovery — using DNS record types to enumerate services). In ordinary Wi-Fi, a device browsing for, say, AirPlay receivers multicasts an mDNS query and receivers answer with the relevant DNS records. The records that matter:

- **PTR** — "instances of this service type exist; here are their names." (Service enumeration.)
- **SRV** — "this named instance lives at this host and port." (Connection details.)
- **TXT** — "here is arbitrary key/value metadata about the instance." (Capabilities, flags.)
- plus the **A/AAAA** address records and a hostname.

Normally this is a request/response exchange over multicast. AWDL does something cleverer.

## Offloading DNS-SD into action frames

Instead of running a separate discovery conversation, **AWDL staples the Bonjour records directly onto the action frames it is already broadcasting** for synchronization and election. Recall the relevant TLVs from Chapter 3:

- **Service Parameters TLV (type 6)** — service-discovery parameters.
- **Service Response TLV (type 2)** — carries the actual **PTR, SRV, and TXT** resource records. It appears only in MIFs and **may be present multiple times** (one MIF can advertise several records).
- **Arpa TLV (type 16)** — carries the **hostname** in reverse-DNS form.

Because every node is *already* periodically broadcasting MIFs to participate in election and synchronization, piggybacking the service records onto them means **discovery costs no extra round trips**. The moment a node changes what it's advertising — say, an Apple TV starts offering AirPlay, or your phone opens the AirDrop share sheet — the new records go out in the next MIF and nearby peers know immediately. There's no "send query, wait for response" latency; the advertisement *is* the broadcast the protocol was sending anyway.

This is a genuinely elegant fusion: the same frames that keep the cluster's clocks aligned also carry its service catalog. It's also why AirDrop *feels* instant — by the time two devices are synchronized, they've already exchanged service records in the same breath.

One cost worth noting, consistent with AWDL's general character: this offloading is **redundant and verbose**. When AirDrop is active, the same service instance string and device name can appear **three times** in a single frame. AWDL trades frame bloat for simplicity and immediacy throughout, and service discovery is no exception. (For space reasons the original SEEMOO paper didn't fully dissect every byte of the service-discovery encoding, but the structure — DNS-SD records carried in repeating Service Response TLVs — is well established, and the Wireshark dissector decodes them.)

## Addressing: the problem

Once a node has discovered a peer's service, it needs to open an actual network connection — which means it needs an IP address to talk to. AWDL carries **only IPv6** (Chapter 3). So each AWDL node needs a working IPv6 address on its `awdl0` interface, and a way for peers to learn it.

Two complications make this non-trivial:

1. **MAC randomization.** For privacy, AWDL does **not** use the Wi-Fi chip's fixed hardware MAC address. Instead it generates a **random MAC address every time the interface is activated.** This is a real privacy win — it prevents trivially tracking a device across AWDL sessions by its burned-in address — but it means addressing can't assume a stable hardware identity.
2. **Avoiding handshake overhead.** The normal way to resolve link-layer-to-IP mappings in IPv6 is the **Neighbor Discovery Protocol (NDP)**, which involves solicitation/advertisement exchanges. That's extra round trips on a link that's trying to be fast and that already broadcasts everyone's source address in every frame.

## Addressing: the solution — derive IPv6 from the MAC

AWDL sidesteps NDP entirely with a neat shortcut. Every action frame already contains the sender's **source MAC address** in its 802.11 header. So instead of *resolving* addresses through a protocol, AWDL **derives** each peer's link-local IPv6 address deterministically from that source MAC, using the standard method from **RFC 4291** (the modified EUI-64 construction for IPv6 link-local addresses).

The construction, for a 48-bit MAC `o0:o1:o2:o3:o4:o5`:

```
fe80::(o0 XOR 0x02):o1:o2:ff:fe:o3:o4:o5
```

Mechanically: split the 48-bit MAC in half, insert the fixed bytes `ff:fe` in the middle (expanding 48 bits to a 64-bit interface identifier), flip the universal/local bit (the `XOR 0x02` on the first octet), and prepend the `fe80::` link-local prefix. This is the textbook EUI-64 link-local derivation; AWDL's only twist is *when* it applies it — to the source address of a received action frame.

The payoff is immediate: **the instant a node receives its first action frame from a peer, it can compute that peer's link-local IPv6 address and add it (with the MAC) straight to the system neighbor table — no NDP, no ARP, no resolution round trip.** Discovery and addressing collapse into a single step. You hear a peer's frame, you derive its IPv6 address, and you can open a socket to it. The OWL implementation does exactly this: on receiving an action frame, it derives the link-local IPv6 from the source Ethernet address and adds both to the neighbor table.

Because the source MAC is randomized per activation, the derived IPv6 address is likewise fresh per session — privacy preserved — while still being instantly computable by anyone who hears the frame.

## A wrinkle: the real MAC leaks anyway

Here's a tension worth flagging, because it foreshadows Chapter 9's privacy discussion. AWDL randomizes the MAC *of the AWDL interface* — but the **Data Path State TLV (type 12)** announces, among other things, the **real (non-randomized) MAC address of the Wi-Fi chip** as well as the BSSID of the infrastructure network the node is currently associated to. So the very same protocol that carefully randomizes the link-layer address for privacy also broadcasts the hardware's true MAC in a TLV. The randomization protects against one kind of tracking (following the `awdl0` address) while the Data Path State TLV potentially undermines it (exposing the stable hardware address). This is the sort of inconsistency that only surfaces when you read the actual protocol rather than the privacy marketing — and it's exactly the kind of thing security researchers pounced on.

## How it fits together

Trace the path from "two synchronized radios" to "an app socket," and service discovery plus addressing is the connective tissue:

1. Nodes are synchronized and hopping channels (Chapters 4–6), broadcasting MIFs.
2. Those MIFs carry **Service Response TLVs** (Bonjour PTR/SRV/TXT) and an **Arpa TLV** (hostname). A browsing peer reads them and learns *what services exist and how they're named* — **instantly**, with no separate query. **(Service Discovery.)**
3. The same received frames carry the sender's source MAC. Each peer applies the RFC 4291 EUI-64 derivation to get the sender's **link-local IPv6 address** and populates its neighbor table — **instantly**, with no NDP. **(Addressing.)**
4. The application now has everything it needs: a service name, a port (from the SRV record), and a routable IPv6 address. It opens an ordinary IPv6 socket over `awdl0`, and from here on it's just TCP/TLS/HTTPS like any other network connection — which is precisely where Chapter 8 picks up with AirDrop.

The design philosophy is consistent with everything we've seen: **eliminate round trips by piggybacking on broadcasts the protocol already sends.** Synchronization frames carry service records; source MACs become IPv6 addresses by formula. Discovery and addressing, which in a conventional stack are separate protocols with their own handshakes, here become free side effects of the frames AWDL is broadcasting anyway. That's why the whole experience feels like there was no setup at all — from the user's perspective, there wasn't.

---

*Next: Chapter 8 — AirDrop itself, finally, in full: the BLE trigger, the HTTPS-over-AWDL application protocol, the contact-identifier authentication, and the two privacy flaws (plus the PSI-based PrivateDrop fix) that this chapter's MAC and contact-hash leaks have been quietly setting up.*
