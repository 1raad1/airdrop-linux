# Chapter 5 — Master Election and the Sync Tree

Chapter 4 established that AWDL's timing depends on a single node — the master — emitting a clock that everyone follows. This chapter answers the obvious next questions: *who* becomes master, *how* that's decided, what happens when two clusters collide, and how the system survives the master walking out the door. The answers are a study in pragmatic minimalism. Where you might expect a sophisticated, metric-driven leader election that picks the "best" node, you instead find something close to a random draw — and yet, paired with a few simple robustness rules, it works well enough that you've used it hundreds of times without noticing.

## The master's job (and only its job)

It's worth restating, because it bounds everything: in AWDL the master has exactly **one** responsibility — to emit the timing signal that synchronizes availability windows across the cluster. It is *not* an access point. It does *not* relay other nodes' data. It does *not* gate-keep membership. This is a deliberate and consequential difference from Wi-Fi Direct, where the Group Owner behaves like an AP and relays traffic, so that losing it collapses the group. In AWDL, because the master only provides a clock, losing it is a much smaller event: data continues to flow directly between peers, and any node can step in to provide the clock.

In a two-node cluster, one node is master and the other is a slave. In larger or multi-hop clusters, intermediate nodes can act as **non-election masters** — relays that repeat the top master's clock signal downstream — but there is always exactly **one top master** in a cluster.

## The election metric — and the punchline that it's random

Election is decided by a single value: the **master metric**, carried in the **Election Parameters v2 TLV**. The rule is as simple as it gets: **the node advertising the highest metric becomes master.**

Now the surprise. Apple's own patents suggest this metric *could* encode something meaningful — available battery, CPU load, signal strength, the sort of thing a thoughtful leader election would weigh. In the actually deployed protocol, the metric is **simply chosen at random**. There is no cleverness. A node picks a random number in a range and that's its claim to the throne.

The lifecycle of a node's metric goes like this:

1. On activating its AWDL interface, a node sets its metric to a fixed initial value of **60** and listens on the social channels for an existing master for about **2 seconds**.
2. Whether or not it found one, after that listening period it draws a **random** number from a predefined range and adopts that as its metric.

The "predefined range" is the one genuinely clever part, because it's **version-dependent**:

- AWDL **v2.x**: range **405–436**
- AWDL **v3.x**: range **505–536**

Because a v3 node always draws from a strictly higher band than a v2 node, **the newest-version device in a cluster is essentially guaranteed to win the election and become master.** This is almost certainly intentional: it ensures the master speaks the most up-to-date dialect of the protocol, so any newer TLVs or behaviors are available cluster-wide and backward compatibility flows downhill from the most capable node. It's a neat way to encode "most modern node leads" into what is otherwise a random draw — partition the random ranges by version.

The low initial value of 60 also has a purpose: because a freshly joined node briefly advertises only 60 before drawing its real (much higher) metric, it won't accidentally seize mastership the instant it appears. This damps needless re-elections when new devices join.

## A consequence: masters get "overtaken"

One genuinely interesting empirical finding falls out of the random metric. Because every node — even one that has *already found* an existing master — still proceeds to draw its own random self-metric from the version range, a node can end up with a *higher* metric than the sitting master purely by chance and **take over the master role**, even though a perfectly good master already existed.

SEEMOO observed exactly this: in a cluster of an iMac, iPhone, MacBook, and iPad, the iMac created the cluster and was master; the iPhone joined and took over (higher metric); then the iPad briefly adopted the existing master but, having drawn an even higher self-metric, immediately seized mastership itself. Meanwhile the older-version iMac and MacBook were only ever eligible to be master when no newer-version device was present.

The lesson about the implementation's character: it's **simplistic**. A node doesn't really reason "is there already a master? then stand down." It keeps its initial metric only briefly, then grabs a random value from its version's range regardless, and the chips fall where they may. It works because correctness doesn't depend on *which* node is master, only on there being exactly one — and the version-banded ranges guarantee the winner is always a most-modern node.

## Merging two clusters

What happens when two independently-formed clusters — each with its own master — drift into range of each other? They need to merge so that nodes across the former boundary can discover and reach one another.

The mechanism is elegantly trivial, and it falls straight out of the metric design. Every node continuously advertises its current cluster's **top master metric** in its Election Parameters. So when a node from cluster A hears a node from cluster B, it immediately learns B's top master metric. Whichever master has the **higher** metric wins; nodes on the losing side adopt the winning master. The merge propagates outward: the first boundary node to adopt the new (higher) master starts advertising that metric, its neighbors hear it and follow, and the wave ripples through the lower cluster until both are one. No special "merge protocol," no negotiation — just the same highest-metric-wins rule applied continuously, which makes cluster fusion a natural consequence of normal operation rather than a separate feature.

## The Synchronization Tree and loop prevention

In a multi-hop cluster, the master's clock is relayed through intermediate non-election masters, forming a **tree** rooted at the top master. Trees built by distributed nodes that are each independently choosing whom to follow can develop **loops** — A follows B, B follows C, C follows A — which would be disastrous for a timing hierarchy.

AWDL prevents this with the **Synchronization Tree TLV (type 20)**. Each node's action frame includes the **full list of nodes on the path from itself up to the top master**. Before a node adopts some other node as its (non-election) master, it checks that tree: if it finds *itself* already in the candidate's path to the top, adopting that candidate would close a loop, so it refuses. This same mechanism bounds the maximum depth of the election tree. It's a clean, stateless loop-avoidance scheme: every frame carries its own provenance, and you simply never adopt a parent who is already downstream of you.

(File away, again, that this TLV is a **variable-length list of addresses parsed in the kernel**. Chapter 9 returns to the consequences when an attacker controls that length.)

## Surviving master churn

The final robustness property — and the one that makes AWDL feel solid in real use — is its tolerance of the master simply **disappearing**. There is no graceful sign-off message. A master that leaves (you turn off Wi-Fi, you walk away) just *stops sending action frames*.

So how does the cluster notice? Purely by timeout. If no frames arrive from the master for a fixed **"no master" timeout of 96 AWs (≈ 1.5 s)**, the other nodes conclude the master is gone. Another node then takes the master position. And here's the graceful part: because every node was **already synchronized to the departed master**, the survivors do *not* need to re-synchronize from scratch. They simply adopt the new master, whose clock is (by construction) the same clock they were already following. Communication is not interrupted; there's no group teardown and rebuild.

Contrast this sharply with Wi-Fi Direct, where the Group Owner is effectively an AP that relays traffic, so its departure forces a full group re-establishment. AWDL's "master only emits a clock" design is precisely what makes master churn a non-event. A leaving master doesn't break links; it just hands off a clock that everyone already knows.

## RSSI hysteresis: avoiding "master flapping"

One last refinement keeps the election stable in the face of noisy radio conditions. Signal strength (RSSI) is used to filter out flaky neighbors: AWDL drops frames whose RSSI is below an **edge sync threshold** of about **−65 dBm** (or **−78 dBm** when AirPlay is in use). The point is to avoid synchronizing to a peer whose link is too marginal to be reliable.

But applying that threshold uniformly would cause a problem: a master at the edge of range whose RSSI fluctuates around the threshold would have its frames intermittently dropped, repeatedly triggering the no-master timeout and forcing the cluster to keep re-electing — "master flapping." To prevent this, frames **from the current master** get a **bonus of 5** (a lower effective threshold), so the master is cut more slack than a random peer. This hysteresis lets the master's RSSI wobble a bit without the cluster panicking and re-electing. It's a small, sensible piece of control-system damping bolted onto the election to keep it from oscillating.

## The whole picture

Assemble the pieces and AWDL's election is revealed as a minimal, robust machine:

- **Who leads:** whoever draws the highest random metric — with version-banded ranges ensuring it's always a newest-version node.
- **Why random works:** correctness needs only *one* master, not the *best* master; identity is irrelevant.
- **Stability:** an initial low metric of 60 plus RSSI hysteresis prevents needless and oscillating re-elections.
- **Merging:** continuous highest-metric-wins makes two clusters fuse with no special protocol.
- **No loops:** every frame carries its full path to the top master; never adopt a parent already below you.
- **Churn tolerance:** a vanished master is detected by a ~1.5 s timeout, and because the master only ever supplied a clock everyone already had, succession is seamless.

It is not elegant in the academic sense — a reviewer would flag the random metric and the redundant self-promotion as crude. But it is *robust* in the engineering sense, which is the only sense that ships on a billion devices. And it's a recurring pattern in AWDL: the protocol's safety comes not from sophistication but from making the failure modes cheap.

---

*Next: Chapter 6 — channel sequences, the formula that maps a window number to a channel, and the real coexistence story of sharing one radio between AWDL and your home Wi-Fi (with the throughput numbers to prove it).*
