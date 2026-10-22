---
status: open
kind: defect
opened: 2026-07-30
---

# Memory exhaustion must preserve system control

## Agreed ownership and pressure behavior

A job and its helpers share accounting. Independently launched jobs have separate
owners under explicit launch authority; ordinary spawn does not create extra
entitlement. Package identity, permissions and accounting remain distinct.
CPU fairness is flat across jobs with the approved per-CPU approximation.

Protect control/cleanup capacity; applications share the remaining RAM without
default per-app quotas. One app may consume it, preventing another from starting.
Reclaim safe caches, then refuse new commitments. Do not kill unrelated apps for
pressure. Infallible application allocation may still end that application.
Account for kernel allocations induced by userland. Local finite bounds remain
until allocation refusal and protected capacity replace their safety function.
Swap and container resource hierarchies are deferred; avoid ownership escapes
through child creation without implementing those future facilities now.

## Memory migration contract

- Ordinary pages are 4 KiB. Large virtual ranges need no physically contiguous
  backing. One mapping inventory supports subrange operations.
- Separate reservation, strict commitment and lazy population. Successful
  anonymous commitment includes backing and later page-table/fault metadata
  needs. Decommit may retain the reservation. Ordinary Rust contracts stay intact.
- Allocation charges have a fixed owner and survive its creator while another
  holder retains the backing. Sharing counts once; copies are new allocations.
  Mapping metadata/page tables are charged per mapper. Shared immutable file
  backing has one shared-files account without its own fixed quota or reserve.
- Prefer client-provided bulk buffers. Services own/bound their internal state;
  no automatic cross-account allocation or exact attribution of every service
  byte to a client. Sharing grants no authority to create new owner charges.
- Commit selected stack capacity before thread start; populate lazily with guards.
  Keep caller-selected sizes and normal thread-creation refusal.
- Support controlled executable-memory transitions without a separate user prompt,
  including writable/executable alias protection and sealed shared executables.
- Coherent mutable shared-file mappings and files larger than RAM are required.
  Demand-page, reclaim clean pages and write back dirty ones; never commit the
  whole file or charge its backing again per mapper. Fileserver owns consistency
  with ordinary I/O and durability; kernel supplies generic memory mechanisms.
- Unlink preserves existing handles/mappings until last release. Fileserver death
  invalidates affected shared mappings; subsequent access may end the app. Fresh
  opens use the restarted server. Unsaved writes may be lost, durable acknowledgments
  must hold. No transparent orphan adoption or uncertain write replay.
- Irrecoverable mapped-page I/O failure may terminate the affected app with file/
  offset diagnostics. Slow storage/pressure alone do not. An overdue page request
  remains waiting with visible progress diagnostics; no automatic hang restart.
- Explicit shared child mappings may request a virtual address before start;
  refuse conflicts without replacing existing mappings or repeated spawn attempts.
  Preserve authority, accounting and protection. PostgreSQL uses native spawn;
  no address-space cloning solely for its port. SQLite uses its ordinary Unix
  adaptation over libc, including shared-memory locking and durable flushes.

The design must establish reclaim/writeback progress without spending protected
control capacity, including release only after every mapping and pin is gone.
No application-specific kernel paths or speculative swap/container machinery.

## Current defect

Application-driven allocation can exhaust memory needed for kernel work,
system controls and cleanup. Ownership accounting and protected reserve/refusal
paths are missing. Failure of another app's new allocation is allowed by the
policy; loss of the system's control and cleanup paths is the defect.

Thread creation is one arm of it, carried out of the closed unbounded-list
panic work: nothing caps the machine's live thread count
either — `MAX_SYSINFO_THREADS` bounds `SYS_SYSINFO`'s reporting vector, not
the threads themselves, so a spawn loop is stopped only by running the
machine out of kernel stacks.

**The class statement, which is what predicts the next instance.** *A client's
request is an allocation request, and every one of them needs an owner who can
say no.* Three instances were filed under it — the compositor's windows, netd's
piped connections, and `SYS_CONNECT` pinning 4 MiB into an unbounded pending
queue — and all three now have a bound *and* a caller that hears the refusal,
which is the pair the class asks for: `toyos_desktop::max_windows` and netstack's
`places_for` each divide an eighth of physical memory by what one unit
costs and refuse past it, and a pipe allocates its ring page on first use rather
than at `create`. **What a bound alone still does not answer is whose window to
refuse.** The memory is charged to nobody, so a cap is the only thing between one
client and the machine. A third such cap
is the kernel's directory numbering, `MAX_DIRECTORIES` in `kernel/pure/dirnodes.rs`,
and its refusal crosses processes: once one process's `/tmp` names fill it, every
process's `stat`, `access`, `opendir` or listing that reaches a directory not yet
numbered is refused `ResourceExhausted`. Attribute each allocation to its owner,
but remove a local cap only when actual allocation refusal and reserve
protection replace its function. Accounting alone removes no bound; retain
finite protocol and queue limits needed for bounded work.

Completion requires allocation refusal before application-driven exhaustion
can disable system controls or cleanup, together with ownership accounting.
Accounting alone does not establish that protection.
