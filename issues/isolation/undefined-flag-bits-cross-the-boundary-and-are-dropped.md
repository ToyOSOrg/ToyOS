---
status: open
kind: defect
opened: 2026-08-31
---

# Four syscalls accept a flag bit they do not define, and serve the request anyway

A caller that sets a bit this kernel does not implement is asking for something
it will not get. Four places take the word, ask `contains` for the bits they
know, and drop the rest — so the caller is answered as though it had asked for
something else, with nothing said.

* **`SYS_MMAP`.** `sys_mmap` (`kernel/src/arch/syscall/vm.rs:19`) reads
  `MmapProt::WRITE` at `vm.rs:30` and `MmapFlags::FIXED` at `vm.rs:27` and asks
  nothing else. `MmapProt` defines `NONE`/`READ`/`WRITE`
  (`toyos-abi/src/syscall.rs:560-562`), `MmapFlags` defines
  `ANONYMOUS`/`PRIVATE`/`FIXED` (`:577-579`); every other bit of both `u64`s is
  accepted and dropped. `prot = READ | 4` is served as a read-only mapping, and
  a caller that believed bit 2 meant "executable" gets one that is not.
* **`SYS_OPEN`.** `dispatch.rs:128` wraps the raw third argument as
  `OpenFlags(a3)` and `ops::open` (`kernel/src/object/ops.rs:70-74`) asks for
  `WRITE`, `CREATE`, `TRUNCATE` and `APPEND`. Five bits are defined
  (`toyos-abi/src/syscall.rs:531-535`); `READ` is never asked either, so the
  word is unvalidated in both directions.
* **`SYS_PROCESS_WAIT`.** `sys_process_wait` (`kernel/src/arch/syscall/proc.rs:66`)
  is `flags & WNOHANG == 0` and nothing more. `WNOHANG` is bit 0
  (`toyos-abi/src/syscall.rs:293`); the other 63 are dropped.
* **The inbox submission, twice over.** `WatchFlags::from_raw`
  (`kernel/src/inbox.rs:119`) keeps the caller's whole `u32` and answers only
  bits 1 and 4, so an `OP_WATCH` asking for a third interest is registered for
  neither. And `Submission::flags: u8` (`toyos-abi/src/inbox.rs:35`) is read by
  **nothing** — `git grep` finds no reader in the kernel — which is a whole
  declared field a caller can set with no effect at all.

`SYS_NAMESPACE_BUILD` is the one that does not: `NamespaceBuild::flags` has a
`NAMESPACE_FLAGS_KNOWN` mask and `sys_namespace_build` refuses
`InvalidArgument` for anything outside it, before a pointer is read. Found
while looking for an in-tree precedent for that refusal, and finding these four
instead.

## What closing it takes

A `KNOWN` mask beside each flag word, and each site refusing `InvalidArgument`
for a word carrying anything outside it — before the site's other checks, so
the answer does not depend on which refusal is reached first. `userland/libc`
and the std fork must be audited for a bit they pass today that the kernel does
not define; a caller found passing one is the interesting half of the work, not
an obstacle to it. `Submission::flags` is the one that may end as a deletion
rather than a mask: a field nothing reads is not a flag word yet.

The instrument is one guest arm per site: the call with a bit outside the mask
must answer `InvalidArgument`, and the same call without it must succeed, so
the refusal is the bit and not the request. `endowment_denied`'s
`the_base_plus_one_more_name` is that arm for `SYS_NAMESPACE_BUILD` and is the
shape to copy.
