---
status: open
kind: defect
opened: 2026-08-24
---

# `spawn_thread` panics at one end and refuses at the other for the same missing entry

`process::spawn_thread` asks the process table for the spawning process twice —
once before it builds anything and once under the lock that inserts — and the
two lookups answer a missing entry differently.

Phase 1 (`kernel/src/process.rs`, the block that takes `parent_addr_space`)
takes `Admit::NoSuchProcess` as a kernel panic, which is what the `.unwrap()`
that preceded it did:

```rust
proclife_spawn::Admit::NoSuchProcess => {
    panic!("spawn_thread: pid {parent_process} is spawning and is not in the table")
}
```

Phase 3 (`admit_thread_insert`, four dozen lines below) answers `None`, and
`sys_thread_spawn` turns that into `SyscallError::ResourceExhausted`.

**One of the two is wrong and the file does not say which.** Either a running
thread's own process can be reaped out from under it here — in which case phase
1 is a userland-reachable kernel panic, which the fail-fast rule forbids at a
trust boundary — or it cannot, in which case phase 3's refusal is an arm
nothing reaches and `mark_thread_zombie`'s neighbouring comment ("silent about
an entry that has gone: a main thread reaches this after its own process
published its exit") is the false one.

This is the same disagreement `thread_exit` carried one function over, and
`thread_exit` has since been settled the tolerant way: a reaped entry routes to
`teardown::ThreadExit::Gone` and the thread finishes leaving. That is a choice
of arm and not the argument this file asks for — the question in both is whether
a thread can reach a syscall body after `kill_process` on another CPU has
published its process's exit and an idle pass has taken the entry, and nothing
yet answers it.

**What a fix owes.** Not a choice of arm — an argument. Whichever way it goes,
one sentence at the site saying whether the entry can be missing, and the two
phases made to agree with it.

Found while extracting the lifecycle's decisions into `toyos-proclife`
(2026-08-24), which is what made the two spellings of one lookup visible: they
are now one function asked twice, and the callers still disagree about its
`NoSuchProcess` answer.
`toyos_proclife::interleave::tests::a_thread_exit_that_outlived_its_entry_still_leaves`
holds the schedule that reaches the state at `thread_exit`. **It reaches it by
routing rather than by running**: the model's `retire` takes a thread off every
CPU, so no schedule it can enumerate has a live thread arriving at a syscall
body with its entry gone. That gap is what leaves this file's question open —
the model cannot exhibit the state the kernel would have to survive.
