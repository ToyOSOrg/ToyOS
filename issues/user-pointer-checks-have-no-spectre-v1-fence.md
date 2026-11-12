---
status: open
kind: defect
severity: [security]
opened: 2026-09-29
---

# User-pointer checks have no Spectre v1 fence

**Built** (#1150): every user copy goes through `arch::uaccess`, whose
routines replace a user address at or past the half's top with 0 without a
branch (`cmovae` on x86-64; `csel` and `csdb` on AArch64), so a mispredicted
range check in `user_ptr` loads no kernel byte through a user pointer. Linux
at `Ubuntu-6.8.0-142.142` reports `usercopy/swapgs barriers and __user
pointer sanitization` for its own user-address masking. The swapgs half is
`arch::entry`'s: each gate's `swapgs`, conditional on its frame's `CS`, is
followed by an `LFENCE` before any load through GS (Intel, "Speculative
Behavior of SWAPGS and Segment Registers", ID 758371), which on AMD orders
dispatch only where `LFENCE` is dispatch-serializing (AMD 70586 G-2).

**Left:** the measurement the exit asks for. No gadget has been run against
these routines or that fence on any proving machine.

**Exit**: on every proving machine a gadget through each `user_ptr` site, and
one through a gate's `CS` test, recovers a planted byte no more often than
chance, 1 in 256, by a bound the stage derives over 1000 trials from the same
gadget's unfenced rate on that machine. **Mutation**: a site's fence or mask
removed exceeds the bound.
**Oracle**: the CPU's own speculation.
