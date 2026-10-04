---
status: open
kind: defect
opened: 2026-07-30
---

# No physical memory fairness

Any process can allocate unbounded physical memory until the system runs out.
No per-process limits, no memory pressure signals, no OOM killer. A single
misbehaving process starves everything.

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
which is the pair the class asks for: `toyos_desktop::max_windows` and netd's
`max_piped_connections` each divide an eighth of physical memory by what one unit
costs and refuse past it, and a pipe allocates its ring page on first use rather
than at `create`. **What a bound alone still does not answer is whose window to
refuse.** The memory is charged to nobody, so a cap is the only thing between one
client and the machine — which is this entry, and is why both of those functions'
doc comments name a kernel memory limit as what deletes them.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling: isolation
findings are security-adjacent and are promoted, never folded). One process
starving every other of physical memory is a denial of service across the
isolation boundary that needs no privilege and nothing crafted — it is real
today, not a possibility. Owed by whoever builds per-process physical
accounting; nothing in the tree measures or refuses it now.
