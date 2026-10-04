---
status: open
kind: defect
opened: 2026-09-29
---

# `test_rs_mutual_kill` panicked on the T14: stdio slot 1 "holds a region that is not a log ring"

The full metal run of `wt/toyos-metaltimings` at `db5e59bc4` failed
`test_rs_mutual_kill` in the `shared-2` boot; the run of the same branch at
`09fe6121e` passed it. `git diff --stat 09fe6121e db5e59bc4` names one file,
`tests/metal/lenovo-20w0003amz.toml`, which the host harness reads and no image
carries. Unexplained: nothing below says why.

## What the log shows

The job was spawned at 3.045 s as pid 28 and spawned its children, pid 29 to
156, each ending `code=137` or `code=0`. The two exit records written just
before the panic:

    [2026-09-29 18:49:34 10.010 cpu4] exit: test_rs_mutual_kill pid=155 code=0 cpu=10ms
    [2026-09-29 18:49:34 10.010 cpu5] exit: test_rs_mutual_kill pid=156 code=137 cpu=10ms

Then pid 28 itself, under the runner's name:

    {2026-09-29 18:49:34 10.011 error pid=28 test-runner} thread 'main' (1) panicked at /Users/jan/Dev/jan/toyos-phdr/toyos/src/log/stdio.rs:209:33:
    {2026-09-29 18:49:34 10.011 error pid=28 test-runner} stdio: slot 1 holds a region that is not a log ring
    ...
      11:      0x1000006e6be - toyos[db68fa2b5c494fe7]::log::stdio::target::sink
      12:      0x1000006e7c2 - toyos[db68fa2b5c494fe7]::log::stdio::target::write
      13:      0x100000568d8 - <alloc[58e89b8cb86be8e3]::io::buffered::bufwriter::BufWriter<std[cb898f2ab581ea90]::io::stdio::StdoutRaw>>::flush_buf
      ...
      18:      0x10000063bc3 - std[cb898f2ab581ea90]::io::stdio::_print
      19:      0x10000028f12 - mutual_kill[652ee4c06bb687b]::main
    [2026-09-29 18:49:34 10.085 cpu5] exit: test_rs_mutual_kill pid=28 code=101 cpu=5840ms

`toyos/src/log/stdio.rs:209` is `sink`'s panic on the first write to a
stream whose slot `ask` refused, with `NotARing`'s words (`:276`). The path is
the sysroot's source, which is keyed by content and so names whichever
worktree built that key first.

## Exit condition

Why pid 28's slot 1 answered `NotARing` on its first `println!` is named with
evidence and removed, and `test_rs_mutual_kill` passes on a T14 run.
