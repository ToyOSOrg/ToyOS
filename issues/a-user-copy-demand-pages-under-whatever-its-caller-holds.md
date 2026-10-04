---
status: open
kind: finding
opened: 2026-10-04
---

# A user copy demand-pages under whatever its caller holds

`user_ptr::translate_user` (`kernel/src/user_ptr.rs`) calls
`process::handle_page_fault` directly when a user address is not mapped, and
every user copy faults its windows in through it (`fault_in`).
`handle_page_fault` takes `PROCESS_TABLE`, the current process's
`process_data` and its address-space lock, all three a `sync::Lock`. A syscall
that copies user memory while holding any of the three re-enters that ticket
lock on the first unmapped window: it spins, and the kernel panics
"DEADLOCK at …" (`Lock::lock`, `kernel/src/sync.rs`). Nothing in the tree
refuses that order: no lock-order check covers these locks.

**Exit:** the order is refused by something that fails (a type or a
checked lock level), or every caller is shown to hold none of the three and
the invariant is one line in `user_ptr.rs`'s header.
