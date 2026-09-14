---
status: open
kind: defect
opened: 2026-09-14
---

# Nothing bounds the log's writer in the window below the boot's last word

`kernel/src/arch/syscall/machine.rs`'s `quiesce` stops every userland thread
before it syncs anything — except the holders of the log capability.
`log::wait_for_durable` waits for the boot's last word to reach `/log`, and the
only writer of that file is a userland process, so a carved-out process runs on
from before `sync_all` until after `log!("Rebooting.")`, and
`quiesce::stop(Stage::All)` takes it at the far end of that window. **Nothing
bounds what it does inside it.**

## The two things the writer can still do

**Write pages no sync reaches.** `logd`'s durability is its own: it writes a
batch, `fsync`s it, and publishes the timestamp only after that call returns
(`userland/logd/src/main.rs`'s two publish sites; `kernel/src/arch/apic.rs`'s
`owed` states the same order). Everything through the last word is therefore
durable by construction. A batch it *starts* after that publish is not — and
those pages belong to a file that is still open, which `writeback::drain_all`
(closed files only) and `Vfs::sync_all` (mount metadata) both pass over, so a
second sync below the last word would not reach them either.

**Put a record under the boot's last word.** The second stage runs below
`log!("{last}")` of necessity, and a syscall the carved-out process is already
inside runs to its end. `kernel/src/object/ops.rs`'s `fsync` retry loop logs
`fsync: … durable on attempt N` on every attempt past the first, so a `/log`
slow in exactly this window writes a kernel record beneath `Rebooting.`.

## The carve-out is the capability, which is wider than the wait

The kernel names the carve-out by the right `/system/bin/init` moved into a
process, because the alternative — the process that last moved
`log::user::durable_ns` — is a pid any holder of that right nominates itself
with through a syscall word, which is authority self-claimed rather than moved
in by a parent.

The right is wider than the wait. Ten committed configs grant `logread` to two
programs:

    for f in $(git grep -l logread -- '*/system.toml' 'system.toml'); do
      grep -c 'syscap = \[.*logread' $f; done | grep -c '^2$'
    10

On `tests/testcases` and `tests/metalcase` the second holder is `test-runner`,
which writes files and spawns, so on those boots two processes run across
`sync_all` where the wait depends on one. That is narrower than `main`, where
every process does, and wider than the wait needs.

## What has been seen

Neither of the two, on a stick. The first has a signature: T14 run 48 left `1
cluster(s) from 71 are marked allocated and no directory entry reaches them`
with `FSI_Free_Count` one out — `logd` data on the device whose directory entry
was not. That boot never reached `quiesce` at all (it wedged;
`issues/diagnostics/a-t14-wedge-ran-the-deadline-out-and-sealed-nothing.md`), so
it is the shape this would take and not an occurrence of it.

## What would show it

A boot whose `/log` is slow enough that `logd` is inside a retrying `fsync` when
the durability wait returns, judged on `Rebooting.` still being the last record;
and `toyos-fat32-check` over the stick of a boot whose `logd` was stopped
mid-batch.

**Exit condition**: the carved-out writer is stopped at a point where it has
nothing outstanding, or a reading that says what it leaves behind there is
nothing. Narrowing the carve-out to the writer alone is a separate answer and
costs a right of its own — one `init` moves in from a `system.toml` row naming
the program that writes `/log`, so the kernel still reads a capability and not a
claim. That is an ABI change and lands on its own pull request.
