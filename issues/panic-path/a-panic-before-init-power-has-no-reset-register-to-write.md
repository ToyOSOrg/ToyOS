---
status: open
kind: defect
opened: 2026-09-06
---

# A panic before `init_power` has no reset register to write

The panic path arms a reboot bound (`kernel/src/panic_reboot.rs`) and returns
the machine to firmware through the FADT's reset register when nobody presses a
key. It can only do that once `acpi::init_power` has decoded that register, and
`init_power` runs at the "peripherals ready" boot phase — long after
`percpu::init_bsp`, which is where the owner's T14 stops today. So the panic
this bound was asked to end is precisely the one it cannot end: the panel says
`panic: holding this panel, timed by the TSC frequency CPUID states: this kernel
has decoded no reset register to hand the machine back to firmware with`, and
the machine still needs a hand.

The clock half of the same window is already solved — `clock::cpuid_tsc_hz`
times the bound off CPUID leaf 15H/16H before `clock::init` — so what is left is
one ACPI decode, not a design.

The fix is an init-order change and belongs to whoever owns that order:
`acpi::parse_madt` already reads this machine's tables two lines above
`init_bsp`, so the reset register can be decoded in the same window. Doing it
inside the panic handler instead is refused: walking firmware tables on a
machine that has already failed once is how a panic becomes a triple fault.

Evidence: a `test-early-panic` guest (`screen_early_panic`, `Profile::Gop`,
dev host, 2026-09-06) renders that line on its panel. `panic_reboots` covers the
post-`init_power` half and is green.
