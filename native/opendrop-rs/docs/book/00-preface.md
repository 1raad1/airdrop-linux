# Apple Wireless Direct Link: A Field Guide

*An in-depth, chapter-by-chapter tour of the protocol behind AirDrop, AirPlay, and Apple's peer-to-peer ecosystem — reconstructed from reverse-engineering research, open-source reimplementations, and the exploit literature.*

---

## Who this is for

You are technically comfortable. You know what an 802.11 frame is, you have a rough mental model of the OSI layers, and you are not afraid of a TLV table or a bit of clock arithmetic. You don't need to be a wireless PhD — every concept that matters is built up from first principles — but this guide assumes you would rather read the real mechanism than a marketing summary.

AWDL is a particularly good subject for this kind of deep dive, because it is one of the most widely deployed wireless protocols on the planet (well over a billion devices) and yet it has *no public specification*. Everything known about it comes from people who took it apart: binary analysis of Apple's kernel drivers, packet captures, a leaked Broadcom firmware tree, and a clean-room open-source reimplementation. That makes it an unusually honest case study in how a real proprietary protocol is actually built — with all the redundancy, dead configuration knobs, and pragmatic shortcuts that a clean spec would hide.

## How the book is organized

Each chapter is written to be roughly 15–20 minutes of focused reading and is self-contained enough to stand on its own, though they build in a deliberate order.

- **Chapter 1 — The Problem AWDL Solves.** Why peer-to-peer Wi-Fi is hard on a single-radio device, what alternatives existed (IBSS, Wi-Fi Direct, TDLS, NAN, Bluetooth), and why Apple built something new.
- **Chapter 2 — Architecture and the Five Phases.** The life cycle of an AWDL link: activation, election, synchronization, service discovery, data transfer. Where AWDL lives in macOS/iOS.
- **Chapter 3 — The Frame Format.** Action frames vs. data frames, the fixed header, the TLV system, and the full TLV catalogue. The byte-level reality.
- **Chapter 4 — Time: Availability Windows and Synchronization.** How AWDL slices time, the master clock, the synchronization math, and why the guard interval costs you ~10% of the channel.
- **Chapter 5 — Master Election and the Sync Tree.** The (surprisingly random) election metric, cluster merging, loop prevention, churn handling, and RSSI hysteresis.
- **Chapter 6 — Channel Sequences and Coexistence.** How the channel sequence maps onto AWs, how AWDL shares one radio with your home Wi-Fi, the channel-allocation states, and the throughput cost.
- **Chapter 7 — Service Discovery and Addressing.** mDNS/Bonjour offloaded into action frames, MAC randomization, and how link-local IPv6 addresses are derived without NDP.
- **Chapter 8 — AirDrop on Top of AWDL.** The BLE trigger, the HTTPS/TLS exchange, the contact-identifier authentication, and the two privacy flaws plus the PSI-based fix (PrivateDrop).
- **Chapter 9 — Security and the Attack Surface.** The unauthenticated link layer, the parser attack surface, the famous zero-click radio-proximity exploit, and de-anonymization/tracking.
- **Chapter 10 — Build It Yourself.** OWL and the tooling, how to capture and dissect AWDL, and pointers for experimentation. Plus the full reference list.

## A note on sources and certainty

Because there is no official specification, essentially everything here is a *reconstruction*. Where the research community is confident (frame layout, the synchronization equation, the election behavior), the book states it plainly. Where Apple's intent is genuinely unknown — and there are several such places, because the deployed implementation uses only a fraction of the protocol's configurable machinery — the book flags it as inference rather than fact. That uncertainty is part of the subject, not a defect in the telling.

The backbone references are the TU Darmstadt Secure Mobile Networking Lab (SEEMOO) papers, the OWL open implementation, and Ian Beer's Project Zero exploit writeup. Full citations are at the end of Chapter 10, and each chapter points to the specific works it draws on.

Turn to Chapter 1 when you're ready.
