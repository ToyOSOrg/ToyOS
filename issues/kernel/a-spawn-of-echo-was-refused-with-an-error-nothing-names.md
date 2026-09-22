---
status: open
kind: defect
opened: 2026-09-22
---

# A spawn of `/system/bin/echo` was refused with an error nothing names

Seen once, on CI run `35760805511` attempt 1, shard `guest (10)`, on PR #466's
head `0a6b7da5` — which is `cf715c49` plus a USB/xHCI branch. The name that
carried it is `log_nested_emit` (`--smp 1`, actuator `log-nested-emit`):

```
FAIL log_nested_emit: --smp 1 ["log-nested-emit"]: the log gate exited 1
log-gate: FAILED: the record-making child would not start: other error
```

The child is `/system/bin/echo`, spawned by `userland/test-runner`'s
`log_gate.rs` when the storm produced no poll completion, to make one kernel
record from userland (`process.rs`'s `exit:` line). `std::process::Command::spawn`
returned `Err`, and `{e}` printed **`other error`** — `io::ErrorKind::Other`,
which is what our std gives an errno it has no kind for. So the guest refused
its own spawn and the reason did not survive the message: what the kernel
answered `SYS_SPAWN` with is not in the record, and `e.raw_os_error()` is not
printed anywhere on that path.

**What is not established.** Everything: whether the refusal came from the
kernel or from the loader in std, which errno it was, and whether it is the
same failure as
`issues/kernel/two-processes-started-at-an-entry-point-nothing-had-mapped.md`,
the *other* name on the same CI run, whose guest started two processes at an
unmapped entry point in the same run. Both are "a process would not start" on
one run's two shards; that is a coincidence with a shape, not a link.

**What it is not.** It is not PR #466's, as far as four sessions each way can
say: on that head, four full fast-tier `cargo test` runs on the dev host
(2026-09-22) had `log_nested_emit` green 4/4; with the whole branch reverted
onto `cf715c49` as a checked patch, four more sessions of the same suite had it
green 4/4. The branch touches nothing on the spawn path. Neither arm
reproduced it, so neither arm explains it.

**Exit.** The refusal named. The cheapest step is that the message carry
`e.raw_os_error()` — `other error` is a message that costs a whole run to
learn nothing from — and the next sighting then says which refusal it was.
Until then the rate is one guest in one shard of one run, and its `ALONE`
re-run was green.
