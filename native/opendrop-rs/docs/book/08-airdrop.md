# Chapter 8 — AirDrop on Top of AWDL

Everything so far has been the *substrate*: a synchronized, channel-hopping, self-addressing IPv6 link between nearby Apple devices. AirDrop is the most famous thing built on that substrate, and this chapter follows it end to end — how it's triggered, how the actual file exchange works as ordinary web technology over the AWDL link, and how it authenticates that you're sharing with the right person. That authentication step is where the substrate's privacy quirks (the contact hashes, the leaking MAC of Chapter 7) come home to roost, so the chapter ends with the two well-documented privacy flaws and the cryptographic fix the research community designed.

AirDrop is a clean illustration of the layering: AWDL is the link, and AirDrop is "just" an application protocol — TLS and HTTPS — riding on top of it, exactly as Chapter 7 promised.

## The trigger: BLE as a doorbell

AWDL is expensive to keep running, so AirDrop doesn't. Instead it uses **Bluetooth Low Energy** as a cheap, always-listening doorbell that wakes the expensive Wi-Fi machinery only when needed.

When you open the share sheet, your device starts broadcasting **BLE advertisements** containing **truncated cryptographic hashes of your contact identifiers** — short pieces of the SHA-256 hashes of your phone number(s) and email address(es). Nearby devices, which are passively listening for BLE, hear these advertisements. A receiving device checks whether any of those truncated hashes match a contact in its own address book. If something looks like a potential match (or if the device is in "Everyone" mode), it **activates its AWDL interface** — and only then does the full five-phase AWDL dance from Chapter 2 begin.

So the sequence is: **BLE advertisement (cheap, always on) → potential contact match → AWDL activation (expensive, on demand) → synchronized link → service discovery → IPv6 → HTTPS.** The BLE truncated hashes are the very first thing that leaves your device, and — as we'll see — they're also the first privacy problem.

## The exchange: HTTPS over AWDL

Once AWDL is up and the devices have discovered each other's AirDrop service (via the Bonjour records in MIFs, Chapter 7) and derived each other's link-local IPv6 addresses (via the EUI-64 trick, Chapter 7), the actual transfer is **strikingly ordinary**. AirDrop runs as **HTTPS with TLS 1.2** over the AWDL IPv6 link. The sender connects to a small HTTPS server the receiver is running on its `awdl0` interface, and the protocol proceeds in recognizable web-service terms.

The flow, simplified:

1. **Discover** — sender finds the receiver's AirDrop service and address (per Chapter 7).
2. **Ask / authenticate** — an HTTPS exchange in which the two sides attempt to recognize each other as contacts (the mutual-authentication step detailed below) and the sender announces what it wants to send, including a **thumbnail and metadata** about the file.
3. **Receiver decides** — the receiver's user sees the "Accept / Decline" prompt (with the thumbnail) and answers. The decision is sent back over the same HTTPS connection.
4. **Transfer** — on acceptance, the sender streams the **full file** over the established TLS connection.

The crucial architectural point: **AWDL provides no security of its own.** Chapter 9 will stress that all AWDL action and data frames are sent in the clear, unauthenticated. AirDrop's confidentiality and integrity come *entirely* from the TLS 1.2 layer it runs on top. This is a deliberate separation of concerns — AWDL is a dumb, open transport, and each application layers on whatever security policy it needs. AirDrop wants strong transport security and contact-based authentication, so it brings TLS and its own authentication protocol; a different app (say, remote-controlling a Keynote presentation) might apply a different policy. The link doesn't decide; the app does.

## The authentication: proving you're a contact

The interesting and security-sensitive part is **mutual authentication** — how AirDrop decides whether sender and receiver are *contacts* of each other, which governs the "Contacts Only" mode and the non-anonymous UI (showing names and photos rather than generic device names).

The mechanism is built on **contact identifiers** — your phone numbers and email addresses — and Apple-issued certificates. Simplified: each Apple ID is associated, via an Apple-signed **certificate**, with the contact identifiers (phone/email) registered to it. During the AirDrop exchange, the two devices present these certificates and exchange **SHA-256 hashes of their contact identifiers**. Each side then checks whether the *hashed* identifiers it received correspond to entries in its own address book. If your hashed phone number is in my contacts, I recognize you as a contact, and vice versa — mutual authentication succeeds, and we see each other's real identities.

The use of *hashes* rather than raw identifiers is the privacy-motivated design choice: the idea was that you'd exchange hashes so the raw phone number/email isn't sent in the clear. The problem — the heart of the next section — is that hashing phone numbers and email addresses does **not** actually protect them, because the input space is far too small.

## Privacy flaw #1: the sender always leaks (the strong attack)

The contact-identifier exchange contains a structural leak, identified by SEEMOO and presented at USENIX Security 2021. The strongest version of the attack is devastatingly simple and requires **no prior knowledge of the target**:

Because the **sender** discloses **hashed values of its own contact identifiers** (phone number and email) **every time it scans** for available AirDrop receivers — i.e. every time someone opens the share sheet — an attacker need only **passively listen**. Open a share sheet near an attacker (or have the attacker wait near any crowd of phones), and the device hands over the hashes of its owner's phone number and email **before any authentication or consent**. The attacker doesn't need to be in the victim's contacts; they don't need the victim's number or email in advance. They just collect the hashes nearby devices broadcast.

The reason this matters — why a hash isn't protection — is the small input space. Phone numbers have very low entropy (a phone number is, what, ~10 digits in a structured, predictable format). SEEMOO demonstrated reversing **SHA-256 hashes of phone numbers in milliseconds** using a precomputed **rainbow table** (the follow-on "AirCollect" work). Email addresses are harder but often still recoverable from dictionaries and breach corpora. So "we hashed it" provides essentially no protection for phone numbers: the attacker reverses the hash and recovers the actual number.

The practical implications the researchers spelled out: an attacker could build a **database mapping contact identifiers to physical locations** by deploying "collector" devices, or stage **targeted spear-phishing** ("Hi, is this Alice? I got your number from...") armed with a real phone number harvested out of the air.

## Privacy flaw #2: the receiver can leak too (the weaker attack)

The reverse direction also leaks, though it's a weaker attack. An attacker can **open a share sheet** themselves and see whether nearby devices respond with **their own hashed details**. Receivers disclose their hashed identifiers in response — but this version only yields information **if the attacker's own contact identifier is already in the receiver's address book** (because the protocol's recognition is mutual). So it's conditional and far less general than the sender-side attack, but it's a real leak under that condition.

Between the two, the takeaway is stark: **in the deployed AirDrop protocol, simply having AirDrop discoverable — or simply opening the share sheet — can leak your phone number and email to a passive nearby attacker.** The flaw is in the protocol design (exchanging reversible hashes of low-entropy identifiers), not in a fixable implementation bug, which is why it proved stubborn.

## The fix the researchers built: PrivateDrop and PSI

SEEMOO didn't just report the flaw; they designed and implemented a drop-in replacement called **PrivateDrop**, presented alongside the attack at USENIX Security 2021. The core idea is to replace the naive "exchange hashes and compare" step with a proper cryptographic **Private Set Intersection (PSI)** protocol.

PSI is exactly the right primitive for this problem. It lets two parties, each holding a *set* (here: your contact identifiers, and my address book), compute the **intersection** of their sets — i.e. "which of your identifiers are in my contacts?" — **without either party learning anything about the elements outside the intersection.** Applied to AirDrop: the two devices can determine whether they're mutual contacts *without* either one disclosing any contact identifier (or a reversible hash of one) that the other doesn't already know. The vulnerable hash exchange simply disappears; nothing reversible is ever broadcast.

The engineering challenge PrivateDrop solved was making PSI **fast enough and offline enough** for AirDrop's constraints — it has to run between two resource-limited devices, with no server in the loop, fast enough to preserve AirDrop's instant feel. PSI protocols are not free, and a naive one would add seconds of latency. PrivateDrop used optimized PSI (with Apple-style signed certificates binding identifiers to records, to prevent simply lying about your set) and demonstrated a native iOS/macOS implementation with an **authentication delay well below one second** — fast enough to preserve the user experience. The implementation was published openly.

## The aftermath

The disclosure timeline is part of the story and worth stating plainly, because it's a notable case of a structural flaw resisting a fix. SEEMOO **privately disclosed** the vulnerabilities to Apple in **May 2019**, and shared the full **PrivateDrop** solution in **October 2020**. As of the 2021 publication, Apple had **not indicated** it was adopting PrivateDrop or any equivalent mitigation, meaning users remained exposed to the contact-leak attacks. (Apple has since made various changes to AirDrop's discoverability and behavior over subsequent OS versions; if you need the current status of the contact-hash leak specifically, it's worth checking the latest iOS security documentation rather than assuming, since this area has continued to evolve and regional events have prompted further changes to AirDrop defaults.)

## What AirDrop teaches about the stack

AirDrop is the clearest demonstration of AWDL's design philosophy and its consequences:

- **Layering done right:** AWDL is a dumb open transport; AirDrop brings its own TLS and authentication. The link doesn't dictate policy. This is clean engineering.
- **Power pragmatism:** BLE as the cheap doorbell waking expensive AWDL on demand — elegant, and the reason your battery survives.
- **The cost of bolting privacy onto a low-entropy identifier:** hashing a phone number feels like protection and provides almost none. The flaw lived not in AWDL's link layer but in the *application's* authentication design — a reminder that a secure transport doesn't make an application private.
- **The gap between "a fix exists" and "a fix ships":** the research community produced a deployable, fast, open PSI-based replacement; deployment is a separate, slower, vendor-controlled matter.

AirDrop, in short, is where the abstract substrate becomes a product you use — and where the substrate's open, broadcast-everything nature becomes a concrete privacy question. That same openness is the subject of the next chapter, generalized from privacy to the full security picture: an unauthenticated link layer parsed in the kernel, and what attackers have done with it.

---

*Next: Chapter 9 — the security model in full. The completely unauthenticated link, the kernel parser as attack surface, the famous zero-click radio-proximity exploit, and de-anonymization/tracking.*
