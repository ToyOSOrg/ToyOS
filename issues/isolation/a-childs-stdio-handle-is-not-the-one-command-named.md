---
status: open
kind: defect
opened: 2026-09-01
---

# A child's stdio slot is not the handle `Command` named for it

Found while building a guest arm for
`sys/stdio/toyos.rs`'s error mapping, which needs a child whose own stdin or
stdout the parent has staged. It could not be staged, twice, and the second one
is a capability statement rather than a plumbing one.

**A handle passed as `Stdio::from(…)` does not become the child's slot.** A
pipe's *write* end was handed to a child as its stdin
(`Command::new(..).stdin(Stdio::from(write_end))`). The kernel refuses a read
of a handle carrying no `Rights::READ` — `HandleTable::get_ref` returns
`HandleError::Rights`, which `refuse_as_error` turns into `PermissionDenied`.
The child's `std::io::stdin().read()` **succeeded**, so slot 0 was something
else it could read:

```
exit: test_rs_stdio_refusal_child pid=17 code=4 cpu=4ms
```

`code=4` is the probe's "the read succeeded" arm.

**And a dropped `ChildStdout` does not end the pipe for the child holding the
other end.** With `.stdout(Stdio::piped())`, the parent dropped its
`ChildStdout` — the only read end — before releasing the child. The child's 64
writes of 4096 bytes all returned `Ok`, 262144 bytes in all, and the kernel
counted every one of them:

```
syscalls: pid=16 total=72 syscall_wall=0ms 0=64 1=1 6=1 14=1 63=1 72=1 73=2 91=1
exit: test_rs_broken_stdout_child pid=16 code=3 cpu=11ms
```

`pipe::try_write` answers `PipeWrite::BrokenPipe` on `readers == 0` before it
looks at ring space, so the pipe still had a reader. In the same test the
parent writing into a pipe whose reader *process* had exited did get
`BrokenPipe`, so the kernel's accounting is right for that shape.

Whether these are one defect or two is not established here; both are about a
child not holding what its parent named, which is why they are filed together.
The root cause was not chased — the bundle that found them was closing an
unrelated defect.

**Neither observation reproduces from anything in the tree.** Both probes were
scratch binaries, written to stage a guest arm and deleted with it, so the two
captures above are the whole of the evidence and whoever picks this up rebuilds
them. An earlier revision of this file also read the spawn line's `dst=` field
as a handle slot; it is the destination CPU (`scheduler::enqueue_new` returns
`CpuId`), it says nothing about stdio, and that paragraph is struck.

## What closing it takes

Deciding first whether `Stdio::InheritPipe` reaches `SYS_SPAWN`'s slot map at
all for a program the launcher answers `NotDeclared` for, since that is the
path both probes took. The instrument is a guest arm that hands a child a
handle it must not be able to use and asserts the refusal, which is the shape
`handle_kill_policy` already has for handles a process does not hold.
