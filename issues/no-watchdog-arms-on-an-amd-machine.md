---
status: open
kind: defect
opened: 2026-10-12
---

# No watchdog arms on an AMD machine

The loader and the kernel arm only an Intel PCH's TCO timer
(`kernel/src/arch/x86_64/watchdog.rs`, `toyos_tco`). On the AMD bench both say
there is none — `watchdog: no function on bus 0 carries a TCO block this
loader knows` and `watchdog: no PCI function here carries a TCO block this
kernel knows — not armed` (PR #874's whole-suite run on that bench) — so a
frozen boot there waits for a hand on the power button, and the
`loader_watchdog_arms` metal row is red there. AMD's fusion controller hub
carries a watchdog timer of its own; its register map is unread here.

Owned by `issues/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md`,
whose ruling is a watchdog on every machine.

**Exit**: an AMD machine's boot arms its chipset's watchdog, and a row on the
AMD bench reads it armed.
