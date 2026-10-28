---
status: open
kind: defect
opened: 2026-10-28
---

# The AMD bench's C3 is withheld for want of a coherence statement

The AMD bench's `_CST` names three states: C1 as FFH `MWAIT 0`, C2 as a
read of port 0x414 (type 2, 400 µs) and C3 as a read of 0x415 (type 3,
350 µs). acpiserver hands the kernel C2 and withholds C3
(`system/acpiserver/src/cst.rs`), so that bench idles no deeper than 0x414.

**Why.** ACPI 6.5 §8.1.4 makes OSPM responsible for the caches' coherency in
a type-3 state, on a multiprocessor by flushing them before entry. ToyOS
does not flush: `WBINVD` on Zen 2 writes back and invalidates the whole
CCX's shared L3 under its running siblings. Neither PPR 55922, 55901,
55772, OSRR 56255 nor APM Vol. 2 says that an I/O-trapped C-state
(`CStateBaseAddr`, MSRC001_0073) keeps caches coherent on its own. AMD's
Family 16h BKDG (52740 §2.5.3.2.3) does say so of the same interface on that
family: a core in such a state either has its probes serviced or flushes
its L1 in hardware before CC6. That family's BIOS requirement declares its
I/O C-state type 2. It is another family's document, so it is not taken as
Zen 2's.

The same firmware's `_LPI` declares 0x415 with no context to lose (18 and
350 µs wake for 0x414 and 0x415, 60 and 1000 µs residency). OSPM uses `_LPI`
only once `\_SB._OSC` has claimed support for processor containers, and
never mixed with `_CST` (§8.4.3.5). ToyOS claims no such support.

**Exit**: either an AMD document for family 17h saying that its I/O C-states
keep caches coherent with no OS flush, after which `cst.rs` hands over a
type-3 SystemIO state on that family and the AMD's metal row reads 0x415
entered; or ToyOS's `_LPI` support (`issues/aarch64-cpus-idle-only-in-wfi.md`
owes the same decode and processor containers) used on that bench in
place of `_CST`. **Mutation**: the type-3 rule removed, so 0x415 is handed
over and the host test in `cst.rs` reds.
