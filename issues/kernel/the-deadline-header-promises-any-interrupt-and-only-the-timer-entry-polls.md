---
status: open
kind: defect
opened: 2026-09-16
---

# `deadline.rs`'s header promises the bound to any interrupt, and only the timer entry polls it

`kernel/src/deadline.rs:11-13`: "So this one is armed off the parameter line
and polled from the timer interrupt entry, on every CPU, in both rings. Its
whole requirement is that some CPU still takes an interrupt".
`kernel/src/hardlockup/mod.rs:4-5` repeats it: "`crate::deadline` is the bound
on a *machine*, and its whole requirement is that some CPU still takes an
interrupt: it is polled from the timer entry."

`poll` says the narrower thing (`deadline.rs:185-186`): "Whether this
machine's bound has passed; the timer interrupt entry's, in both rings, and
nothing else's." Its call sites are exactly two, both in
`kernel/src/arch/idt/timer.rs`: the Ring 0 naked entry's `call {deadline}`
(`:71`, `deadline = sym crate::deadline::poll` at `:79`, placed there because
"a CPU spinning on a ticket still takes this interrupt", `:61-63`) and
`timer_handler`, which opens at `:92`: its first statement is
`crate::irq_census::irq_took!(Timer);` at `:93`, and `crate::deadline::poll()`
at `:96` is the second. `rg -n 'deadline::poll' kernel/src` finds no third. The same module already states the true requirement once, in
`armed`'s doc (`deadline.rs:86-87`): "Both bounds rest on some CPU taking a
timer interrupt".

So a CPU taking xHCI interrupts, IPIs, or the hard-lockup detector's own NMI
sample every second (`hardlockup/mod.rs:9-14`) — each an interrupt the
header's sentence counts — never reaches `poll`. The header claims coverage
of a machine on which some CPU takes *an* interrupt; the code covers a
machine on which some CPU takes a *timer* interrupt. A T14 boot on
2026-09-14 is the measured gap between the two
(`issues/kernel/a-120000-ms-boot-deadline-fired-132859-ms-late-on-the-t14.md`):
a census in which cpu0 had taken 1729 xHCI interrupts and 3 NMIs against 9
timer ticks, and a bound that fired 132859 ms late. Whether that boot's 133 s
were a machine with no timer interrupt on any CPU is unmeasured; that such a
machine is outside what the code does and inside what the header says is not.

**Exit condition**: the header and the call sites say the same thing —
either `:11-13` and `hardlockup/mod.rs:4-5` say "timer interrupt" and the
machine that takes interrupts but no timer tick is listed under "What it does
not cover" (`:17-32`), or `poll` is reached from every interrupt entry and
`:185-186` says so.
