---
status: open
kind: defect
severity: [hang]
opened: 2026-11-12
---

# A panic's end is unread on either bench

On x86-64 a fatal path holds its report for the panic bound, from the boot's
first fatal event, and then ends the machine. The bound is timed by the
calibrated clock, the TSC rate CPUID 15H states, or a ceiling on any TSC's
rate. The end is the FADT's reset register, learned right after the boot
parameter, and the processor's shutdown where no register is known or the
register leaves the machine running a second later. QEMU's q35 resets on
both (`an_early_panic_resets_at_its_bound`,
`a_panic_with_no_reset_register_shuts_down`).

**Neither bench has shown a panic end itself.** The panics recorded on them
needed a hand: #886's two negative-control arms, which panicked at the
microcode read-back inside `after_console`. The register is the one
a job list's `reboot` writes on both (the T14's 0xcf9 <- 0x06, the AMD bench's
`SMI_CMD` 0xb0 <- 0xfb), but no panic before `interrupts` reached it until the
register moved ahead of `after_console`. What a processor shutdown does is
the chipset's to configure: on AMD's Family 19h Model 11h the FCH answers a
CPU's SHUTDOWN message with an INIT upstream or a PCI reset, INIT at its reset
value (PPR 55901 Vol. 5, §10.1.17 and `PMx08[20]`); the AMD bench's own PPR,
55922 for Family 17h Model 60h, has no FCH chapter, and no Intel document read
here says what the T14's PCH does. The AMD bench states no TSC rate, so its
hold before the clock lasts 60 s times 10 GHz over its TSC's rate.

The boot after a panic reports it only once a record crosses the reset:
`issues/a-crash-record-survives-a-reset-outside-ram.md`.

An AArch64 machine with no PSCI has no end a fatal path can take, and holds
its panel for good.

- **Owed on metal:** a boot armed with `test-early-panic`, on the T14 and on the
  AMD bench, with nobody at the machine. Main must show each machine back
  without a hand: the T14 after its 60 s bound timed by CPUID 15H, the AMD
  bench after its ceiling's bound, each through its reset register.
- **Owed on metal:** a boot armed with `reset-withheld` and `test-early-panic`,
  on the T14 and on the AMD bench. Main must show whether each comes back from
  the processor's shutdown alone, and what the AMD bench's ceiling bound took.

**Exit**: both boots on both benches come back with nobody at the machine, or
the run shows the end that did not, read off the machine; and the boot after
each reports the panic once the crash record's route lands.
