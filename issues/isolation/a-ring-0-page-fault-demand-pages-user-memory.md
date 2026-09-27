---
status: open
kind: defect
opened: 2026-09-27
---

# A Ring 0 page fault demand-pages user memory

`page_fault_handler` (`kernel/src/arch/x86_64/idt/exceptions.rs`) resolves a
not-present fault through `process::handle_page_fault` for a Ring 3 frame or
for any Ring 0 frame with a thread current. No kernel path takes a Ring 0 fault
on a user address on purpose: `user_ptr::translate_user` demand-pages
explicitly and every copy goes through the direct map. So the Ring 0 arm only
ever serves a kernel bug, and it serves it by mapping a page into whichever
process is current — an interrupt handler's stray access fills a bystander's
address space. SMAP then faults the retry, but `cr4::SMAP` is optional in the
declaration (`kernel/src/arch/x86_64/control_regs.rs`), and on a CPU without it
the access succeeds silently.

**Evidence:** read from the code; no test stages it.

**Exit condition:** a Ring 0 not-present fault on a user address is never
resolved and goes to `blame`, gated by a guest test.
