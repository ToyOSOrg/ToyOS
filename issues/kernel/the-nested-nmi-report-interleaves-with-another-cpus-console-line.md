---
status: open
kind: defect
opened: 2026-10-01
---

# The nested-NMI report interleaves with another CPU's console line

`nested_nmi` (`kernel/src/arch/x86_64/idt/nmi.rs`) wrote its report through
`serial::panic_raw`, which took no lock. With cpu1 writing its own record at
the same moment, the two lines interleaved byte by byte on the 16550. The cpu1
line was `[kernel 0.385 cpu1] CPU 1: joining scheduler`, and the 16550 carried:

    [[kenrnmel i0.38]5  cpNu1E] CSPUT 1E: Djo inNiMngI s choednule r

`NESTED NMI` was never whole on the console, and the machine halted with its
report unreadable.

#675 (`bc68e5d78`) writes the report under the console's registers
(`serial::panic_registers`).

**Evidence:** red under KVM in two runs:
- Main's nightly `guest` lane at `06788146b`, run 36843762360, job 110374194368.
- PR #671's `guest` check, on its merge onto `59052827f`, run 36863809437, job
  110375742604.

It is green under TCG: on the dev host in a whole-suite run of #670's branch,
and on a runner in main's nightly `tcg` lane at `06788146b` (job 110374194382).

**Exit:** `nested_nmi_is_loud` is green in CI's KVM `guest` check, and a report
written while another CPU is writing a record reads whole.
