---
status: open
kind: defect
opened: 2026-08-24
---

# A syscall the dispatch refuses is counted but never timed

Noticed while splitting `kernel/src/arch/syscall.rs` into
`kernel/src/arch/syscall/`, not while looking for it.

`syscall_dispatch` counts every call before the match and times it after:

- **before** — `data.syscall_total += 1` and `data.syscall_counts[…] += 1`, so
  a number this ABI does not issue is counted too, deliberately;
- **after** — `let elapsed = nanos_since_boot() - t0; data.syscall_total_ns +=
  elapsed`.

Between them are **58 `return` statements** (`grep -c 'return '` over the
body of `syscall_dispatch` in `kernel/src/arch/syscall/dispatch.rs`), which is
every argument refusal the ABI has: a `user_bytes` window that would not
translate, a `user_str` that was over-long or not UTF-8, a `checked_mul` that
wrapped, a `UserAddr::checked` that was not in the user half, a capability that
did not carry its bit. Each of them leaves the function past the counter and
before the clock.

So `ProcessStats::syscall_total` and `ProcessStats::syscall_total_ns` do not
describe the same set of calls, and the average a reader divides out of them is
of the calls that were *not* refused, against a count that includes the ones
that were. A process being hammered with bad arguments looks cheap.

Neither field has a gate, and nothing in the tree reads their ratio today —
which is why this is a finding rather than a defect. What makes it worth a file
is that it is invisible at the site: the counter and the clock are 650 lines
apart in the one function that has to hold them, and the arms between them look
like ordinary refusals.

## The other half of the same exit is already filed

The same 58 returns also skip `object::drain_zero_handles()`, the first of the
object layer's three drain sites. That one is materially harmless and already
recorded: the drain also runs at the top of every scheduler pass and from the
idle loop, and `issues/kernel/deferred-release-outlives-its-syscall.md` carries
the measurement of what a release outliving its syscall costs. Nothing here
adds to it.

## What a fix would have to decide first

Whether a refused call is *work*. Counting the argument check as syscall time
is defensible and so is not counting it; what is not defensible is the two
fields disagreeing about which question they answer. Deciding that is cheaper
than the change: the mechanical part is one scope — the match's result and the
epilogue in a closure, or the refusals returning a value the epilogue sees
instead of returning past it.

**2026-08-25, promoted to `defect`.** Re-verified on this tree after the split
finished: `kernel/src/arch/syscall/dispatch.rs` increments the counter at line
137 and adds the elapsed nanoseconds at line 779, and `grep -c 'return '` over
the file still answers **58**. Two `ProcessStats` fields describing different
sets of calls is a wrong number a reader cannot detect from the outside, which
is a defect rather than an observation. Owed by whoever next touches the
dispatch epilogue: decide whether a refused call is work, and make both fields
answer that one question.
