---
status: open
kind: defect
opened: 2026-10-01
---

# A fatal report written raw can splice into another CPU's console burst

`serial::panic_raw` takes no lock, so a report written through it while
another CPU writes the console lands on the 16550 inside that CPU's burst, byte
by byte. `nested_nmi` holds the registers for its report
(`serial::panic_registers`); these do not:

- `panic::last_words`, on both of its arms, `PANIC REENTRY` and `DOUBLE
  PANIC`, written before any CPU is stopped. The reentry arm never stops them.
- `percpu::ist1_report`, after `halt_all_cpus`' flush. A CPU that spun for the
  registers with `IF` clear through the flush takes them as the flush ends,
  and its burst goes out beside the `[ist1]` line before it takes the halt IPI.

Taking them in `last_words` with `panic_registers` is not the fix: a reentry
inside `panic_flush`'s drain would find them held by its own CPU's outer frame,
and wait the whole bound for a holder that never runs again. `panic_flush` and
`nested_nmi` already pay that wait when the hold is their own CPU's.

**Evidence:** the code. CI's KVM `guest` lane recorded the mechanism on
`nested_nmi`'s report before it held the registers: run 36863809437, job
110375742604.

**Exit:** every raw report holds the registers for its whole length, and no
fatal path waits out a hold its own CPU left.
