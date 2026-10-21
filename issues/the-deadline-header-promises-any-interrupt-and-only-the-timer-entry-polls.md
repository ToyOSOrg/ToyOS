---
status: open
kind: defect
opened: 2026-09-16
---

# `deadline.rs`'s header promises the bound to any interrupt, and only the timer entry polls it

`kernel/src/deadline.rs`'s header: "So this one is armed off the parameter
line and polled from the timer interrupt entry, on every CPU, in both rings.
Its whole requirement is that some CPU still takes an interrupt".
`kernel/src/hardlockup/mod.rs`'s header repeats it: "`crate::deadline` is the
bound on a *machine*, and its whole requirement is that some CPU still takes
an interrupt: it is polled from the timer entry."

The calls say the narrower thing. `poll` and `poll_in_kernel` are reached from
the timer entries alone: x86-64's Ring 0 naked entry and `timer_handler`
(`kernel/src/arch/x86_64/idt/timer.rs`), and AArch64's timer arm
(`kernel/src/arch/aarch64/trap.rs`); `rg -n 'deadline::poll' kernel/src` finds
no other. The same module already states the true requirement once, in
`armed`'s doc: "Both bounds rest on some CPU taking a timer interrupt".

So a CPU taking device interrupts, IPIs, or the hard-lockup detector's own NMI
sample every second (`hardlockup/mod.rs`'s header) — each an interrupt the
header's sentence counts — never reaches `poll`. The header claims coverage
of a machine on which some CPU takes *an* interrupt; the code covers a
machine on which some CPU takes a *timer* interrupt. A T14 boot on
2026-09-14 is the measured gap between the two
(`issues/a-120000-ms-boot-deadline-fired-132859-ms-late-on-the-t14.md`):
a census in which cpu0 had taken 1729 xHCI interrupts and 3 NMIs against 9
timer ticks, and a bound that fired 132859 ms late. Whether that boot's 133 s
were a machine with no timer interrupt on any CPU is unmeasured; that such a
machine is outside what the code does and inside what the header says is not.

**Exit condition**: the headers and the call sites say the same thing —
either both headers say "timer interrupt" and the machine that takes
interrupts but no timer tick is listed under `deadline.rs`'s "What it does not
cover", or `poll` is reached from every interrupt entry and its doc says so.
