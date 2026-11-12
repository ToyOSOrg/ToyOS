---
status: open
kind: defect
opened: 2026-10-10
---

# An executable's RELRO stays writable for its whole life

**Built**: `toyos::dl::start` makes an executable's whole `PT_GNU_RELRO`
pages read-only with `SYS_VM_PROTECT` once every relocation is applied, as
the SDK's loader does a library's. `relro_sealed` stores to a relocated word
in a whole RELRO page from a child and holds that the child ends as a memory
fault. The AArch64 job case (`virt_jobs_at_el2`) runs it, and the same row
without the seal is red. On x86-64 it is a member of the T14's `testcases`
shared boot, which no guest row runs.

**Left**:
- **Owed on metal:** the T14's `testcases` shared boot. Main must show the
  whole boot green, every discovered Rust test and every C corpus case among
  it; the line `relro_sealed: a store to a relocated word of PT_GNU_RELRO
  ended its program as a memory fault`; no program saying `PT_GNU_RELRO could
  not be made read-only`; and no exit 127 that main's last `testcases` boot
  before the seal did not have.

Exit: once a program has relocated itself its `PT_GNU_RELRO` range is
read-only, through a protection primitive the program calls or a mapping
granular enough to hold it apart, and a guest test's write to it is refused.
