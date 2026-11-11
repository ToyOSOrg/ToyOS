---
status: open
kind: track
opened: 2026-09-29
---

# blockd drives NVMe APST

After blockd drives the T14's drive, the blockd step of
`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`. The NVMe
base specification defines APST: enabled through Feature 0Ch, the controller
moves itself, once idle for the time a power state's table entry names, to
the non-operational state that entry names, one with no higher idle power;
and the drive reports each state's worst entry and exit latency, a
transition costing the old state's exit latency plus the new one's entry
latency (NVM Express Base Specification 2.1, §5.1.25.1.6, §8.1.17 and
§8.1.17.2). blockd builds the table from the power state descriptors of the
drive's own `id-ctrl`. The specification bounds no latency: blockd's bound is
its design round's, chosen on its merits. It follows the specification
with no table of drives: a drive that misbehaves under it is a defect,
measured on the bench and filed.

**Built** (#1131): the NVMe driver sets the table, the same one Linux sets
on both benches' drives; no back-to-back disk row regressed on either
(evidence `pr-1131` r10). The idle minute was measured in r1 to r4 and did
not fall on the T14, for reasons that are
`issues/the-t14s-idle-package-stays-in-pc2.md`'s. The first write after a
wake is `issues/the-first-durable-write-after-the-drive-wakes-is-a-millisecond-behind-linuxs.md`'s.

**Exit**: host tests of the APST table; on the
T14, the one proving machine with a physical NVMe drive, the block figures and
the idle minute with the drive idle. **Mutation**: a table that admits a state
past the bound. **Oracle**: the specification's rules for the table over the
T14's `id-ctrl`.
