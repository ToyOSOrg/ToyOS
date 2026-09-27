---
status: open
kind: finding
opened: 2026-09-27
---

# A test's doc says a zero-size `PT_TLS` gets a DTV slot; the kernel gives it none

`toyos-elf/tests/crafted.rs`'s `a_zero_size_tls_segment_is_present_not_absent`
says "a module with a `PT_TLS` of zero size still gets a DTV slot". The kernel
gives a zero-`memsz` segment no TLS module and so no module id
(`TlsSegment::occupied`, applied in `kernel/src/loader/tls.rs`).

Exit condition: that doc line is deleted.
