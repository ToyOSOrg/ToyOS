---
status: open
kind: defect
opened: 2026-10-12
---

# A wake storm costs the T14 twice the AMD bench's time per waiter, and nothing says why

`wake_storm_cost` times one `Watch::post_n` over 1 to 64 waiters with
`rdtsc` around the syscall (`tests/guest/rust-tests/src/bin/wake_storm_cost.rs`).
On `main`'s kernel the per-waiter term reads:

| bench | head | TSC | per waiter | one waiter |
|---|---|---|---|---|
| T14 | `c126cd9d5` | 2419 MHz | 812 cycles, 335 ns | 9405 cycles |
| AMD bench | `fb03703a1` | 1796 MHz | 299 cycles, 166 ns | 4608 cycles |

(T14: https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6114829103;
AMD bench: https://github.com/ToyOSOrg/ToyOS/pull/852#issuecomment-6107031259.)
An earlier T14 boot read 746 cycles, 308 ns
(https://github.com/ToyOSOrg/ToyOS/pull/592#issuecomment-5969968549). Each
figure is one boot's minimum of nine, so the spread across boots is unknown.

The walk claims each waiter and posts to its home CPU's mailbox and doorbell
(`kernel/pure/sched/mailbox.rs:474-490`), kicking one that sleeps. The T14 runs
its local APIC in x2APIC mode (`apic=X2apic`,
https://github.com/ToyOSOrg/ToyOS/pull/861#issuecomment-6113257693) and the
AMD bench in xAPIC (`apic=Xapic`,
https://github.com/ToyOSOrg/ToyOS/pull/852#issuecomment-6107031099); the T14
has PCID and the AMD bench does not; the two parts move a line between cores
differently. None of these has been measured against the gap. One part of a
kick is now timed: an IPI's send is 189 TSC ticks (78 ns) on the T14 and
under one 18-tick quantum (10 ns) on the AMD bench, and its skew-free delivery
966 (399 ns) against 1062 (591 ns)
(https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119443198). How many
of the walk's waiters cost a kick is unread.

**Exit**: the per-waiter term split on both benches into its parts, the
cause of the gap named by that split, and the T14's figure brought to the
AMD bench's or the remainder recorded here with the hardware bound that
sets it.
