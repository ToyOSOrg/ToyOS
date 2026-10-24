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

**Measured** on the T14 (#1061's diagnosis boots, osbench `wake.storm.call`,
p50 in ns, head `799f4214d` unless named; each cell is one boot):

| | w1 | w4 | w16 |
|---|---|---|---|
| C1E on, the call at once after a 2 ms sleep | 4393, 4819, 4722 | 10122 | 14152 |
| C1E on, the waker spinning 20 µs in Ring 3 first | 3877 | 3876 | 7206 |
| C1E auto-promotion cleared | 841 | 4489 | 8674 |
| `main` `32046264b`, C1E auto-promotion cleared | 871 | 4486 | 14481 |

The spin removes the slow mode at w4 and w16, but not at w1. There the
C1E-cleared reading, 841 ns, is what the first syscall after a wake costs
without the exit. A faster wake path reaches the call sooner after its own
wake, so more of its work runs inside the exit. #1061's `storm.call`
regression against `main` at w4 is this, and not the call's work: no diary
record shows a pick, preemption or interrupt on the waker's CPU inside a call,
and with C1E cleared `main` and the branch read the same. At w16, `main` *rose*
under the control, from 9948 and 10490 with C1E on to 14481 cleared. That
reading is unexplained and comes from one boot, so the branch's lead there rests
on it. Linux on the same machine reads w4 at 11.6 µs.

The fix that keeps a sub-floor deadline from being slept past covers idle CPUs
only. A busy CPU whose parked neighbour's deadline is less than the one-shot
floor away still arms the floor, which is unmeasured.

Owner: `issues/idle-cpus-enter-the-c-states-cst-names.md`, which chooses the
idle states; this is the cost of not choosing any.

**Exit**: an idle policy that does not pay a C1E exit on a short idle, chosen
per Intel's documentation of `MSR_POWER_CTL` and the `_CST` states, measured on
the T14 by `wake.storm.call`, the `time.sleep_late` rows and the idle minute's
package energy against the current policy.
