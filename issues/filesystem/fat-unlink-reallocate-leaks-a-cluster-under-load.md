---
status: open
kind: defect
opened: 2026-08-25
---

# `fat_backing_revoked`'s unlink-and-reallocate leaks a cluster under host load

Seen once, dev host, in a full Fast suite (`cargo test`) run beside seven
`qemu-system-x86_64` guests belonging to two other worktrees on the same
14-core machine. The third-party checker refused the log volume after the
test's own scenario had completed:

```
FAIL fat_backing_revoked: the unlink-and-reallocate cycle left the log volume breaking the format:
1 cluster(s) from 134 are marked allocated and no directory entry reaches them
```

The harness re-ran it alone in the same session and it was green — `PASS
fat_backing_revoked (4s)`, with the test's own verdict line intact: *the victim
is gone from the volume, the 32768-byte file written into its place holds 0x5c
end to end on the host's own reader, and the checker is silent*. The rest of
the suite was 285 passed, 1 failed, 286 total.

**This is not the setup failure the build tracker carried against this test's
name** (since closed: the setup asks again on a `WouldBlock` from
`fs::File::create` within a stated patience now, instead of panicking on the
first one). This one runs the whole scenario, produces the right bytes,
and leaves one cluster allocated with no directory entry reaching it — a FAT
the driver wrote and `toyos-fat32-check` refuses. The oracle is
Microsoft's fatgen103 checker and not this tree's own reader, which is what
makes the verdict worth acting on rather than re-running away.

Load-coincident, and that is a reason to look rather than a reason to dismiss:
the unlink frees a chain and the create allocates into it, so the window where
one write's budget can expire between the two is exactly what a loaded host
widens. `kernel/CLAUDE.md`'s block-layer rule names the shape — a
`BudgetExpired` that discards its pages instead of re-enqueuing splits a FAT
the moment one copy's write outlives the other's budget — and a leaked cluster
is the same family with one FAT rather than two.

What is owed first is a reproduction that says which side leaks: the free-chain
walk of the unlink, or the allocation the create takes out of it. The suite's
own alone/loaded arms are the instrument, and
`tests/toyos-rust-tests/src/bin/fat_backing_revoked.rs` plus
`tests/common/volumes.rs` are where the scenario and the checker call live.

Found by the comment sweep on `wt/toyos-sw3`, whose diff is comments and
whitespace only and cannot have caused it.
