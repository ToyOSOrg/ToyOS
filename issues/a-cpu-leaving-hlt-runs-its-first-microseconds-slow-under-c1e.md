---
status: open
kind: defect
opened: 2026-10-24
---

# A CPU leaving `hlt` runs its first microseconds slow under C1E auto-promotion

An idle CPU waits in `hlt` (`kernel/src/arch/x86_64/hw.rs`). The T14's firmware
leaves C1E auto-promotion on: every CPU reads `MSR_POWER_CTL = 0x24005b`, bit 1
set. So every `hlt` is promoted to C1E, and a thread woken there runs its first
microseconds at a clock that has not come back yet.

**Measured** on the T14 (#1061's diagnosis boots, osbench `wake.storm.call`, p50, ns):

| | w1 | w4 | w16 |
|---|---|---|---|
| C1E on, the call at once after a 2 ms sleep | 3877–4200 | 10122 | 14152 |
| C1E on, the waker spinning 20 µs in Ring 3 first | — | 3876 | 7206 |
| C1E auto-promotion cleared | 841 | 4489 | 8674 |

The C1E-cleared row comes from head `799f4214d`; base `32046264b` reads the same
there, with w4 at 4486. A faster wake path reaches the call sooner after its
own wake, so more of its work runs inside the exit. #1061's
`storm.call` regression against `main` is this, and not the call's work: no
diary record shows a pick, preemption or interrupt on the waker's CPU inside a
call. Linux on the same machine reads w4 at 11.6 µs.

Owner: `issues/idle-cpus-enter-the-c-states-cst-names.md`, which chooses the
idle states; this is the cost of not choosing any.

**Exit**: an idle policy that does not pay a C1E exit on a short idle, chosen
per Intel's documentation of `MSR_POWER_CTL` and the `_CST` states, measured on
the T14 by `wake.storm.call`, the `time.sleep_late` rows and the idle minute's
package energy against the current policy.
